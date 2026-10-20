//! The Ledger's accounts and journal over RPC (ADR 0014), under `/rpc` next to the REST routes.
//!
//! [`Ledger`] is a `#[rpc::service]`: queries, mutations (each retried safely with an
//! `Idempotency-Key`, kept in PostgreSQL by [`PgIdempotencyStore`]) and one server stream, the
//! statement of an account ([`Ledger::statement`]). Every procedure calls the function its REST
//! route calls ([`accounts::open_account`], [`entries::post_entry`], …), so both surfaces decide,
//! validate and fail alike; REST and its OpenAPI document stay the public API (ADR 0014).
//!
//! **Access.** Every procedure is guarded by the named policy its REST route uses
//! ([`Policy`]: `ledger.read`, `accounts.open`, `ledger.post`), evaluated before the procedure
//! runs; the `/rpc` routes themselves are public at the route level, so the deny-by-default
//! layer serves them and the per-call guard decides. The caller is the principal the
//! authentication layer recorded (session cookie or bearer token), the same one the REST
//! handlers see ([`Caller`]). `/rpc` needs no CSRF token: an RPC `POST` must carry `x-rpc: 1`
//! and a JSON content type, which a cross-site form cannot send (C5).
//!
//! **Errors.** [`LedgerError`] carries what the REST Problem would say: its status picks the
//! variant, its `detail` (and a `422`'s field issues) the fields; a server fault is
//! `InternalError` with a correlation id, never its cause.
//!
//! **Contract.** [`contract`] is checked in as `rpc.contract.json` (`tests/rpc.rs` keeps it
//! current, `UPDATE_GOLDEN=1` rewrites it); CI's `id-effect rpc check` compares it with the last
//! release and checks the TypeScript client generated from it (`web/rpc`,
//! `id-effect rpc gen ts --contract rpc.contract.json --out web/rpc`).

use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::Request;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::IntoResponse;
use axum::routing::MethodRouter;
use id_effect::schema::serde_bridge::unknown_from_serde_json;
use id_effect::schema::{HasSchema, Schema};
use id_effect::{Ctx, DynEnv, Effect, Stream, succeed};
use id_effect_kit::authz::{CurrentPrincipal, Guard, Policies};
use id_effect_kit::axum::RequestCaps;
use id_effect_kit::axum::route::{GuardFuture, Problem, RouteGuard, RouteInfo};
use id_effect_kit::rpc;
use id_effect_kit::rpc::RpcPrincipal;
use id_effect_kit::rpc::WireSchema;
use id_effect_kit::rpc::contract::Contract;
use id_effect_kit::rpc::service::RpcSchema;
use id_effect_kit::rpc::wire::InputIssue;
use serde_json::{Value, json};
use tower::util::MapRequestLayer;

use crate::accounts::{self, Account, AccountChanges, AccountList, NewAccount};
use crate::app::{App, Caller};
use crate::entries::{self, EntryList, NewEntry, PostedEntry};
use crate::error::ApiError;
use crate::idempotency::PgIdempotencyStore;
use crate::reports::{self, Balance, STATEMENT_PAGE, StatementLine};

/// Where the RPC routes are mounted.
pub const RPC_PATH: &str = "/rpc";

/// The input of [`Ledger::open_account`]: the body of `POST /accounts`, decoded by the same
/// schema ([`NewAccount`]'s: a 3-to-8-digit code, a name of 1 to 80 characters, a known kind),
/// which is also its JSON Schema in the contract.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenAccount(pub NewAccount);

impl RpcSchema for OpenAccount {
  fn wire_schema() -> WireSchema<Self> {
    rest_body(
      NewAccount::schema(),
      OpenAccount,
      |OpenAccount(account)| json!({"code": account.code, "name": account.name, "kind": account.kind}),
    )
  }
}

/// The input of [`Ledger::post_entry`]: the body of `POST /entries`, decoded by the same schema
/// ([`NewEntry`]'s: a memo of 1 to 200 characters, 2 to 100 non-zero lines that balance).
#[derive(Clone, Debug, PartialEq)]
pub struct PostEntry(pub NewEntry);

impl RpcSchema for PostEntry {
  fn wire_schema() -> WireSchema<Self> {
    rest_body(
      NewEntry::schema(),
      PostEntry,
      |PostEntry(entry)| json!({"memo": entry.memo, "lines": entry.lines}),
    )
  }
}

/// A REST body's `schema` as an RPC input: decoded with every issue at once, described by the
/// schema's own JSON Schema, encoded by `encode` (the Rust client's side).
fn rest_body<A, I, T>(
  schema: Schema<A, I>,
  wrap: fn(A) -> T,
  encode: fn(T) -> Value,
) -> WireSchema<T>
where
  A: 'static,
  I: 'static,
  T: Send + 'static,
{
  let json_schema = schema.json_schema();
  WireSchema::from_fns(
    move |value| {
      schema
        .decode_unknown_all(&unknown_from_serde_json(value.clone()))
        .map(wrap)
        .map_err(|errors| {
          errors
            .issues
            .into_iter()
            .map(|issue| InputIssue {
              path: json_pointer(&issue.path),
              message: issue.message,
            })
            .collect()
        })
    },
    move |input| Ok(encode(input)),
    json_schema,
  )
}

/// A dot-separated schema path as a JSON Pointer (`lines.0.amount` → `/lines/0/amount`).
fn json_pointer(path: &str) -> String {
  path
    .split('.')
    .filter(|segment| !segment.is_empty())
    .fold(String::new(), |mut out, segment| {
      out.push('/');
      out.push_str(&segment.replace('~', "~0").replace('/', "~1"));
      out
    })
}

/// The input of [`Ledger::update_account`]: the account, the changes and the version they were
/// made against (as `PATCH /accounts/{id}` takes them).
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct AccountUpdate {
  /// The account.
  pub id: i64,
  /// A new name (1 to 80 characters).
  pub name: Option<String>,
  /// Lock or unlock.
  pub locked: Option<bool>,
  /// The `version` the client read; a newer one in the database is a `Conflict`.
  pub version: i64,
}

/// The input of [`Ledger::entries`] (`GET /entries?limit=&offset=`).
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct EntryPage {
  /// 1 to 100 (default 50).
  pub limit: Option<i64>,
  /// From 0 (default 0).
  pub offset: Option<i64>,
}

/// One input issue of a [`LedgerError::Invalid`].
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct FieldIssue {
  /// JSON Pointer into the input (`/name`).
  pub pointer: String,
  /// What is wrong with it.
  pub message: String,
}

/// The typed errors of every [`Ledger`] procedure: the REST Problem's status and detail
/// ([`ApiError::status_detail`]).
#[derive(Debug, PartialEq, rpc::RpcErrorUnion)]
pub enum LedgerError {
  /// Not signed in (`401`).
  #[rpc(status = 401)]
  Unauthenticated {
    /// Why.
    detail: String,
  },
  /// Signed in, not allowed: a policy denied it, or the principal has no organisation (`403`).
  #[rpc(status = 403)]
  Forbidden {
    /// Why.
    detail: String,
  },
  /// No such account or entry in the caller's organisation (`404`).
  #[rpc(status = 404)]
  NotFound {
    /// What was not found.
    detail: String,
  },
  /// The call conflicts with the current state: a stale version, a duplicate code, a balance
  /// left on a closing account, a lost serialization conflict (retry) (`409`).
  #[rpc(status = 409)]
  Conflict {
    /// Why.
    detail: String,
  },
  /// Well-formed but not acceptable: an unbalanced entry, an unknown account (`422`).
  #[rpc(status = 422)]
  Invalid {
    /// Why.
    detail: String,
    /// Each field at fault.
    issues: Vec<FieldIssue>,
  },
  /// A dependency is away; retry later (`503`).
  #[rpc(status = 503)]
  Unavailable {
    /// Which.
    detail: String,
  },
  /// A server fault: answered as `InternalError { id }`, never sent.
  #[rpc(internal)]
  Internal {
    /// What failed (logged with the correlation id).
    detail: String,
  },
}

impl From<ApiError> for LedgerError {
  fn from(e: ApiError) -> Self {
    let issues = match &e {
      ApiError::Invalid(problem) => problem
        .errors
        .iter()
        .map(|issue| FieldIssue {
          pointer: issue.pointer.clone(),
          message: issue.message.clone(),
        })
        .collect(),
      _ => Vec::new(),
    };
    let (status, detail) = e.status_detail();
    match status {
      StatusCode::UNAUTHORIZED => Self::Unauthenticated { detail },
      StatusCode::FORBIDDEN => Self::Forbidden { detail },
      StatusCode::NOT_FOUND => Self::NotFound { detail },
      StatusCode::CONFLICT => Self::Conflict { detail },
      StatusCode::UNPROCESSABLE_ENTITY => Self::Invalid { detail, issues },
      StatusCode::SERVICE_UNAVAILABLE => Self::Unavailable { detail },
      _ => Self::Internal {
        detail: e.to_string(),
      },
    }
  }
}

/// The guard of a procedure: the named policy of the Ledger's registry (the one its REST route
/// names), evaluated for the request's principal before the procedure runs: `401` for an
/// anonymous caller, `403` for a denied one. The registry comes with the request ([`mount`]).
#[derive(Clone, Copy, Debug)]
pub struct Policy(pub &'static str);

/// The policy registry the [`Policy`] guards evaluate, in the request's extensions.
#[derive(Clone)]
struct RpcPolicies(Policies);

impl RouteGuard for Policy {
  fn policy(&self) -> &str {
    self.0
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    Box::pin(async move {
      let Some(RpcPolicies(policies)) = parts.extensions.get::<RpcPolicies>() else {
        // Mounted without `mount`: fail closed.
        return Err(
          Problem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_detail("authorization could not be evaluated")
            .into_response(),
        );
      };
      let guard = Guard::named(policies, self.0);
      guard.check(parts).await
    })
  }
}

/// The Ledger's accounts and journal, version 1 of its RPC contract.
#[rpc::service(version = 1)]
pub trait Ledger {
  /// The organisation's open accounts, by id (`GET /accounts`).
  #[query]
  #[guard(Policy("ledger.read"))]
  fn accounts(&self) -> Effect<AccountList, LedgerError, DynEnv>;

  /// One open account of the organisation (`GET /accounts/{id}`).
  #[query]
  #[guard(Policy("ledger.read"))]
  fn account(&self, id: i64) -> Effect<Account, LedgerError, DynEnv>;

  /// Opens an account owned by the caller (`POST /accounts`).
  #[mutation]
  #[guard(Policy("accounts.open"))]
  fn open_account(&self, account: OpenAccount) -> Effect<Account, LedgerError, DynEnv>;

  /// Renames, locks or unlocks an account; its owner or an organisation owner only
  /// (`PATCH /accounts/{id}`).
  #[mutation]
  #[guard(Policy("accounts.open"))]
  fn update_account(&self, update: AccountUpdate) -> Effect<Account, LedgerError, DynEnv>;

  /// Closes an account that holds no balance; its owner or an organisation owner only
  /// (`DELETE /accounts/{id}`).
  #[mutation]
  #[guard(Policy("accounts.open"))]
  fn close_account(&self, id: i64) -> Effect<(), LedgerError, DynEnv>;

  /// An account's balance (`GET /accounts/{id}/balance`).
  #[query]
  #[guard(Policy("ledger.read"))]
  fn balance(&self, id: i64) -> Effect<Balance, LedgerError, DynEnv>;

  /// A page of journal entry headers, by id (`GET /entries`).
  #[query]
  #[guard(Policy("ledger.read"))]
  fn entries(&self, page: EntryPage) -> Effect<EntryList, LedgerError, DynEnv>;

  /// One journal entry with its lines (`GET /entries/{id}`).
  #[query]
  #[guard(Policy("ledger.read"))]
  fn entry(&self, id: i64) -> Effect<PostedEntry, LedgerError, DynEnv>;

  /// Posts a balanced journal entry (`POST /entries`); send an `Idempotency-Key` to retry it
  /// safely.
  #[mutation]
  #[guard(Policy("ledger.post"))]
  fn post_entry(&self, entry: PostEntry) -> Effect<PostedEntry, LedgerError, DynEnv>;

  /// An account's statement: every posting in order, each with the balance after it, read page
  /// by page while the client consumes it.
  #[server_stream]
  #[guard(Policy("ledger.read"))]
  fn statement(
    &self,
    id: i64,
  ) -> Effect<Stream<StatementLine, LedgerError, DynEnv>, LedgerError, DynEnv>;
}

/// Runs `f` for the call's caller (the principal in the call's environment) on the call's fiber;
/// without a caller it fails as the REST route would.
fn as_caller<A: Send + 'static>(
  f: impl FnOnce(Caller) -> Effect<A, ApiError> + Send + 'static,
) -> Effect<A, LedgerError, DynEnv> {
  Effect::from_ctx(move |ctx: Ctx<DynEnv>| async move {
    let principal = ctx.env().try_get::<CurrentPrincipal>().ok();
    let caller = Caller::new(principal.as_deref()).map_err(LedgerError::from)?;
    ctx.bind(f(caller)).await.map_err(LedgerError::from)
  })
}

/// Runs the environment-free `effect` on the call's fiber.
fn on_call<A: Send + 'static>(effect: Effect<A, ApiError>) -> Effect<A, LedgerError, DynEnv> {
  Effect::from_ctx(move |ctx: Ctx<DynEnv>| async move {
    ctx.bind(effect).await.map_err(LedgerError::from)
  })
}

/// The handlers of [`Ledger`]: the functions the REST routes call.
#[derive(Clone)]
pub struct Procedures {
  app: Arc<App>,
}

impl Procedures {
  /// The procedures over `app`.
  pub fn new(app: Arc<App>) -> Self {
    Self { app }
  }
}

impl Ledger for Procedures {
  fn accounts(&self) -> Effect<AccountList, LedgerError, DynEnv> {
    let app = Arc::clone(&self.app);
    as_caller(move |caller| accounts::list_accounts(&app, &caller))
  }

  fn account(&self, id: i64) -> Effect<Account, LedgerError, DynEnv> {
    let app = Arc::clone(&self.app);
    as_caller(move |caller| accounts::find_account(&app, &caller, id))
  }

  fn open_account(&self, account: OpenAccount) -> Effect<Account, LedgerError, DynEnv> {
    let app = Arc::clone(&self.app);
    as_caller(move |caller| accounts::open_account(&app, &caller, account.0))
  }

  fn update_account(&self, update: AccountUpdate) -> Effect<Account, LedgerError, DynEnv> {
    let app = Arc::clone(&self.app);
    let changes = AccountChanges {
      name: update.name,
      locked: update.locked,
      version: update.version,
    };
    as_caller(move |caller| accounts::update_account(app, caller, update.id, changes))
  }

  fn close_account(&self, id: i64) -> Effect<(), LedgerError, DynEnv> {
    let app = Arc::clone(&self.app);
    as_caller(move |caller| accounts::close_account(app, caller, id))
  }

  fn balance(&self, id: i64) -> Effect<Balance, LedgerError, DynEnv> {
    let app = Arc::clone(&self.app);
    as_caller(move |caller| reports::account_balance(app, &caller, id))
  }

  fn entries(&self, page: EntryPage) -> Effect<EntryList, LedgerError, DynEnv> {
    let app = Arc::clone(&self.app);
    let (limit, offset) = (page.limit.unwrap_or(50), page.offset.unwrap_or(0));
    as_caller(move |caller| {
      if !(1..=100).contains(&limit) {
        return id_effect::fail(ApiError::invalid("/limit", "must be 1 to 100"));
      }
      if offset < 0 {
        return id_effect::fail(ApiError::invalid("/offset", "must be at least 0"));
      }
      let page = id_effect_kit::sql::Page::new(limit.unsigned_abs(), offset.unsigned_abs());
      entries::list_entries(&app, &caller, page)
    })
  }

  fn entry(&self, id: i64) -> Effect<PostedEntry, LedgerError, DynEnv> {
    let app = Arc::clone(&self.app);
    as_caller(move |caller| entries::find_entry(&app, &caller, id))
  }

  fn post_entry(&self, entry: PostEntry) -> Effect<PostedEntry, LedgerError, DynEnv> {
    let app = Arc::clone(&self.app);
    as_caller(move |caller| entries::post_entry(app, caller, entry.0))
  }

  fn statement(
    &self,
    id: i64,
  ) -> Effect<Stream<StatementLine, LedgerError, DynEnv>, LedgerError, DynEnv> {
    let app = Arc::clone(&self.app);
    // The account is checked before the first item: another organisation's is `NotFound`.
    as_caller({
      let app = Arc::clone(&app);
      move |caller| accounts::find_account(&app, &caller, id).map(move |_account| caller)
    })
    .map(move |caller| {
      // The last posting sent; `None` once the statement is complete.
      let after = Arc::new(Mutex::new(Some(0_i64)));
      Stream::from_pull(move |_| {
        let cursor = *after.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(from) = cursor else {
          return succeed(None);
        };
        let after = Arc::clone(&after);
        on_call(reports::statement_page(&app, &caller, id, from)).map(move |lines| {
          let full = i64::try_from(lines.len()).is_ok_and(|n| n == STATEMENT_PAGE);
          let next = lines.last().filter(|_| full).map(|line| line.posting_id);
          *after.lock().unwrap_or_else(PoisonError::into_inner) = next;
          (!lines.is_empty()).then_some(lines)
        })
      })
    })
  }
}

/// Every RPC service the Ledger serves, as one contract document (`rpc.contract.json`).
pub fn contract() -> Contract {
  Contract::new().with_service(LedgerService::contract())
}

/// The RPC routes over `app`, and the routes they declare for the deny-by-default layer.
pub struct Mounted {
  /// `POST /rpc` and `POST` / `GET /rpc/{tag}`.
  pub router: axum::Router,
  /// What [`Mounted::router`] serves (public at the route level; each procedure is guarded).
  pub routes: Vec<RouteInfo>,
}

/// [`Ledger`] over `app` under [`RPC_PATH`]: mutation results kept for `Idempotency-Key`
/// replays in PostgreSQL ([`PgIdempotencyStore`]) and the [`Policy`] guards evaluated against
/// `policies`.
///
/// Each call runs with the request's capabilities (its principal) in its environment, as the
/// REST handlers do: the HTTP transport of `id_effect_rpc` lays them over the route state. Each
/// request also carries the policy registry the guards read and the request's principal
/// (`<org>/<id>`) as the [`RpcPrincipal`] its idempotency keys are scoped by (`scope_call`).
pub fn mount(app: Arc<App>, policies: &Policies) -> Mounted {
  let store = PgIdempotencyStore::new(Arc::clone(&app.db));
  let server = LedgerService::server(Procedures::new(app)).with_idempotency_store(store);
  // The server's routes by path (`POST` and `GET /rpc/{tag}` share one).
  let mut paths: Vec<(String, MethodRouter<DynEnv>)> = Vec::new();
  let mut routes = Vec::new();
  for endpoint in server.endpoints() {
    let (info, methods) = endpoint.into_parts();
    match paths.iter_mut().find(|(path, _)| *path == info.path) {
      Some((_, served)) => *served = std::mem::take(served).merge(methods),
      None => paths.push((info.path.clone(), methods)),
    }
    routes.push(info);
  }
  let policies = policies.clone();
  let router = paths
    .into_iter()
    .fold(axum::Router::new(), |router, (path, methods)| {
      router.route(&path, methods.with_state(DynEnv::new()))
    })
    .layer(MapRequestLayer::new(move |request| {
      scope_call(&policies, request)
    }));
  Mounted { router, routes }
}

/// Adds what an RPC call reads from its request: the policy registry of the [`Policy`] guards,
/// and the authenticated principal as the [`RpcPrincipal`] its idempotency keys are scoped by.
fn scope_call(policies: &Policies, mut request: Request) -> Request {
  let scope = request
    .extensions()
    .get::<RequestCaps>()
    .and_then(|caps| caps.get::<CurrentPrincipal>())
    .filter(|p| p.is_authenticated())
    .map(|principal| match principal.tenant() {
      Some(org) => format!("{org}/{}", principal.id()),
      None => principal.id().to_owned(),
    });
  if let Some(scope) = scope {
    request.extensions_mut().insert(RpcPrincipal(scope));
  }
  request
    .extensions_mut()
    .insert(RpcPolicies(policies.clone()));
  request
}

#[cfg(test)]
mod tests {
  use id_effect_kit::auth::{PasswordError, SessionError};
  use id_effect_kit::authz::{AuthzError, Denial};

  use super::*;

  #[test]
  fn ledger_errors_carry_what_the_problem_says() {
    let detail = |s: &str| s.to_owned();
    let cases = [
      (
        ApiError::Unauthenticated,
        LedgerError::Unauthenticated {
          detail: detail("sign in first"),
        },
      ),
      (
        ApiError::Authz(AuthzError::Unauthenticated),
        LedgerError::Unauthenticated {
          detail: detail("authentication required"),
        },
      ),
      (
        ApiError::NoTenant,
        LedgerError::Forbidden {
          detail: detail("the principal belongs to no organisation"),
        },
      ),
      (
        ApiError::Authz(AuthzError::Denied(Denial::new("not the owner"))),
        LedgerError::Forbidden {
          detail: detail("access denied"),
        },
      ),
      (
        ApiError::NotFound("account"),
        LedgerError::NotFound {
          detail: detail("no such account"),
        },
      ),
      (
        ApiError::Conflict("taken".into()),
        LedgerError::Conflict {
          detail: detail("taken"),
        },
      ),
      (
        ApiError::invalid("/name", "too long"),
        LedgerError::Invalid {
          detail: detail("the request input failed validation"),
          issues: vec![FieldIssue {
            pointer: "/name".into(),
            message: "too long".into(),
          }],
        },
      ),
      (
        ApiError::Session(SessionError::Storage("down".into())),
        LedgerError::Unavailable {
          detail: detail("sessions unavailable"),
        },
      ),
    ];
    for (api, expected) in cases {
      assert_eq!(LedgerError::from(api), expected);
    }
    // A server fault keeps its cause for the log; the client sees `InternalError { id }` only.
    for fault in [
      ApiError::Password(PasswordError::Blocking),
      ApiError::Authz(AuthzError::Evaluation("policy missing".into())),
    ] {
      let text = fault.to_string();
      assert_eq!(
        LedgerError::from(fault),
        LedgerError::Internal { detail: text }
      );
    }
  }

  #[test]
  fn schema_paths_become_json_pointers() {
    assert_eq!(json_pointer(""), "");
    assert_eq!(json_pointer("lines.0.amount"), "/lines/0/amount");
    assert_eq!(json_pointer("a~b.c/d"), "/a~0b/c~1d");
  }

  #[tokio::test]
  async fn a_guard_without_its_registry_fails_closed() {
    let guard = Policy("ledger.read");
    assert_eq!(guard.policy(), "ledger.read");
    let parts = axum::http::Request::new(()).into_parts().0;
    let refused = guard.check(&parts).await.expect_err("refused");
    assert_eq!(refused.status(), StatusCode::INTERNAL_SERVER_ERROR);
  }
}
