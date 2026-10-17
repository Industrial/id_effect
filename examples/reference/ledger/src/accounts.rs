//! Accounts: an organisation's chart of accounts.
//!
//! [`Account`] is an entity: the `Repository` scopes every statement to the caller's
//! organisation (`tenant = "org_id"`), hides closed accounts (`soft_delete = "closed_at"`) and
//! checks the `version` column on every update, so a stale write is a `409`.

use std::sync::Arc;

use axum::Json;
use axum::http::StatusCode;
use id_effect::schema::{self, HasSchema, ParseError, Schema, length, minimum, one_of, pattern};
use id_effect::{Effect, fail, succeed};
use id_effect_kit::authz::Policies;
use id_effect_kit::axum::RequestCaps;
use id_effect_kit::axum::route::{Input, Route, Router};
use id_effect_kit::sql::query_builder::Dialect;
use id_effect_kit::sql::{Entity, FromRow, Repository, params, with_transaction};
use serde::Serialize;

use crate::app::{App, AppEnv, Caller, as_caller, guarded};
use crate::db;
use crate::error::ApiError;

/// One account. `org_id` is the tenant column the repository writes and filters by.
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema, FromRow, Entity)]
#[from_row(crate = id_effect_kit::sql)]
#[entity(
  crate = id_effect_kit::sql,
  table = "accounts",
  tenant = "org_id",
  soft_delete = "closed_at"
)]
pub struct Account {
  /// Assigned by the database.
  #[entity(id, generated)]
  pub id: i64,
  /// The organisation.
  pub org_id: String,
  /// Chart-of-accounts code, unique in the organisation (`1000`).
  pub code: String,
  /// Display name.
  pub name: String,
  /// `asset`, `liability`, `equity`, `income` or `expense`.
  pub kind: String,
  /// Who opened it (ownership: only they or an owner may change or close it).
  pub owner_id: String,
  /// A locked account takes postings from its owner or an organisation owner only.
  pub locked: bool,
  /// Optimistic-locking version.
  #[entity(version)]
  pub version: i64,
}

/// `GET /accounts`.
#[derive(Clone, Debug, Serialize, id_effect::Schema)]
pub struct AccountList {
  /// Open accounts, by id.
  pub accounts: Vec<Account>,
}

/// The account kinds.
pub const KINDS: [&str; 5] = ["asset", "liability", "equity", "income", "expense"];

/// The accounts of `org` (the tenant filter of every statement).
pub fn accounts_of(org: &str) -> Repository<Account> {
  Repository::new(Dialect::Postgres).with_tenant(org.to_owned())
}

/// `{id}` of the account routes: a positive id.
pub struct AccountId;

impl HasSchema for AccountId {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    minimum(schema::i64::<()>(), 1).describe("Account id")
  }
}

/// The body of `POST /accounts`.
#[derive(Clone, Debug, PartialEq)]
pub struct NewAccount {
  /// 3 to 8 digits.
  pub code: String,
  /// 1 to 80 characters.
  pub name: String,
  /// One of [`KINDS`].
  pub kind: String,
}

impl HasSchema for NewAccount {
  type A = NewAccount;
  type I = (String, String, String);
  type E = ();

  fn schema() -> Schema<NewAccount, (String, String, String)> {
    schema::transform(
      schema::struct3(
        "code",
        pattern(schema::string::<()>(), "^[0-9]{3,8}$", |code| {
          (3..=8).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_digit())
        }),
        "name",
        length(schema::string::<()>(), 1, 80),
        "kind",
        one_of(schema::string::<()>(), &KINDS),
      ),
      |(code, name, kind)| Ok::<_, ParseError>(NewAccount { code, name, kind }),
      |a: NewAccount| (a.code, a.name, a.kind),
    )
  }
}

/// The body of `PATCH /accounts/{id}`: the changes and the version they were made against.
/// Fields left out are unchanged (the derived schema accepts a missing `Option` field).
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct AccountChanges {
  /// A new name (1 to 80 characters).
  pub name: Option<String>,
  /// Lock or unlock.
  pub locked: Option<bool>,
  /// The `version` the client read; a newer one in the database is a `409`.
  pub version: i64,
}

/// The open account `id` of the caller's organisation; `404` for any other.
pub fn find_account(app: &App, caller: &Caller, id: i64) -> Effect<Account, ApiError> {
  accounts_of(caller.org())
    .on(Arc::clone(&app.db))
    .find(id)
    .map_err(ApiError::from)
    .flat_map(|found| match found {
      Some(account) => succeed(account),
      None => fail(ApiError::NotFound("account")),
    })
}

fn list(
  env: &mut AppEnv,
  caps: RequestCaps,
  _: Input,
) -> Effect<Json<AccountList>, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  as_caller(&caps, move |caller| {
    accounts_of(caller.org())
      .on(Arc::clone(&app.db))
      .find_all()
      .map(|accounts| Json(AccountList { accounts }))
      .map_err(ApiError::from)
  })
}

fn open(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<(), (), (), NewAccount>,
) -> Effect<(StatusCode, Json<Account>), ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let new = input.body;
  as_caller(&caps, move |caller| {
    let account = Account {
      id: 0,
      org_id: caller.org().to_owned(),
      code: new.code,
      name: new.name,
      kind: new.kind,
      owner_id: caller.id().to_owned(),
      locked: false,
      version: 1,
    };
    // In a transaction: its errors carry the SQLSTATE, so a duplicate code answers 409.
    let accounts = accounts_of(caller.org());
    with_transaction(Arc::clone(&app.db), move |tx| {
      accounts.on(tx).insert(account)
    })
    .map(|account| (StatusCode::CREATED, Json(account)))
    .map_err(ApiError::from)
  })
}

fn show(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<i64>,
) -> Effect<Json<Account>, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  as_caller(&caps, move |caller| {
    find_account(&app, &caller, input.path).map(Json)
  })
}

fn update(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<i64, (), (), AccountChanges>,
) -> Effect<Json<Account>, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let Input {
    path: id,
    body: changes,
    ..
  } = input;
  if let Some(name) = &changes.name
    && !(1..=80).contains(&name.chars().count())
  {
    return fail(ApiError::invalid("/name", "must be 1 to 80 characters"));
  }
  as_caller(&caps, move |caller| {
    find_account(&app, &caller, id).flat_map(move |account| {
      let allowed = caller.authorize(&app.policies, "account.manage", &account);
      allowed.flat_map(move |()| {
        let changed = Account {
          name: changes.name.unwrap_or(account.name),
          locked: changes.locked.unwrap_or(account.locked),
          version: changes.version,
          ..account
        };
        accounts_of(caller.org())
          .on(Arc::clone(&app.db))
          .update(changed)
          .map(Json)
          .map_err(ApiError::from)
      })
    })
  })
}

/// The balance an account holds (sum of its postings), read on the primary.
fn balance_on_primary(app: &App, caller: &Caller, id: i64) -> Effect<i64, ApiError> {
  let sql = "SELECT COALESCE(SUM(amount), 0)::int8 AS balance FROM postings \
             WHERE org_id = $1 AND account_id = $2";
  db::query_one::<(i64,)>(&app.db, sql, params![caller.org(), id])
    .map(|(balance,)| balance)
    .map_err(ApiError::from)
}

fn close(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<i64>,
) -> Effect<StatusCode, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let id = input.path;
  as_caller(&caps, move |caller| {
    find_account(&app, &caller, id).flat_map(move |account| {
      let allowed = caller.authorize(&app.policies, "account.manage", &account);
      allowed
        .flat_map({
          let (app, caller) = (Arc::clone(&app), caller.clone());
          move |()| balance_on_primary(&app, &caller, id)
        })
        .flat_map(move |balance| {
          if balance != 0 {
            return fail(ApiError::Conflict(format!(
              "account {id} holds a balance of {balance}; move it before closing"
            )));
          }
          accounts_of(caller.org())
            .on(Arc::clone(&app.db))
            .delete(id)
            .map(|_closed| StatusCode::NO_CONTENT)
            .map_err(ApiError::from)
        })
    })
  })
}

/// `/accounts` routes.
pub fn routes(policies: &Policies) -> Router<AppEnv> {
  Router::new()
    .route(guarded(
      policies,
      "ledger.read",
      Route::get("/accounts")
        .response::<AccountList>(StatusCode::OK)
        .handler_with_request_caps(list),
    ))
    .route(guarded(
      policies,
      "accounts.open",
      Route::post("/accounts")
        .body::<NewAccount>()
        .response::<Account>(StatusCode::CREATED)
        .handler_with_request_caps(open),
    ))
    .route(guarded(
      policies,
      "ledger.read",
      Route::get("/accounts/{id}")
        .path::<AccountId>()
        .response::<Account>(StatusCode::OK)
        .handler_with_request_caps(show),
    ))
    .route(guarded(
      policies,
      "accounts.open",
      Route::patch("/accounts/{id}")
        .path::<AccountId>()
        .body::<AccountChanges>()
        .response::<Account>(StatusCode::OK)
        .handler_with_request_caps(update),
    ))
    .route(guarded(
      policies,
      "accounts.open",
      Route::delete("/accounts/{id}")
        .path::<AccountId>()
        .handler_with_request_caps(close),
    ))
}
