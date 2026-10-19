//! The RPC surface (`src/rpc.rs`, ADR 0014): the accounts and journal under `/rpc` beside REST,
//! called by the Rust client (`LedgerClient` over HTTP) and by the generated TypeScript client
//! under Deno (`tests/ts/ledger.test.ts`), against a Ledger on loopback over its own database.
//!
//! Deno comes from devenv (`deno` on `PATH`; CI pins it). Without it the TypeScript test prints
//! `SKIP`, unless `ID_EFFECT_REQUIRE_DENO=1`, which turns a missing Deno into a failure.

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::{As, Ledger, with_ledger};
use id_effect::{Effect, HNil};
use id_effect_kit::axum::run_with_env;
use id_effect_kit::platform::error::HttpError;
use id_effect_kit::platform::http::{
  HttpClient, HttpClientConfig, HttpClientService, HttpRequest, HttpResponse, ReqwestHttpClient,
  StreamingHttpResponse,
};
use id_effect_kit::posture::{PROCEDURES_DECLARED, Posture, ProbeStatus};
use id_effect_kit::rpc::RpcClient;
use id_effect_kit::rpc::contract::{ProcedureAccess, assert_snapshot};
use id_effect_kit::rpc::idempotency::{
  Begin, IdempotencyKey, IdempotencyScope, IdempotencyStore, StoredResult,
};
use id_effect_kit::rpc::service::{HttpTransport, RpcCallError, TransportError};
use id_effect_kit::sql::params;
use id_effect_kit::test::http::StatusCode;
use ledger::accounts::NewAccount;
use ledger::entries::{Line, NewEntry};
use ledger::idempotency::PgIdempotencyStore;
use ledger::rpc::{
  AccountUpdate, EntryPage, LedgerClient, LedgerError, LedgerService, OpenAccount, PostEntry,
};
use ledger::{Database, Services};
use serde_json::{Value, json};

/// `rpc.contract.json` is the served contract; `UPDATE_GOLDEN=1 cargo test` rewrites it, and
/// CI's `id-effect rpc check` compares it with the last release.
#[test]
fn ledger_rpc_contract_snapshot_is_current() {
  assert_snapshot(&ledger::rpc::contract(), "rpc.contract.json");
}

#[test]
fn ledger_rpc_procedures_all_declared() {
  // The startup probe over the served contract (C1).
  let report = Posture::new("prod")
    .rpc_service(&LedgerService::contract())
    .evaluate(&[]);
  let probe = report.probe(PROCEDURES_DECLARED).expect("the probe");
  assert_eq!(probe.status, ProbeStatus::Pass, "{report}");
  // Each procedure behind the policy its REST route names; none public.
  let contract = LedgerService::contract();
  let policy = |name: &str| match name {
    "open_account" | "update_account" | "close_account" => "accounts.open",
    "post_entry" => "ledger.post",
    _ => "ledger.read",
  };
  assert_eq!(contract.procedures.len(), 10);
  for procedure in &contract.procedures {
    match &procedure.access {
      ProcedureAccess::Guarded { policies } => {
        assert_eq!(policies, &[policy(&procedure.name)], "{}", procedure.name);
      }
      other => panic!("{} is {other:?}", procedure.name),
    }
  }
}

/// The Ledger of `l`'s configuration over `l`'s database, served on a free loopback port (with
/// the peer address, as `main.rs` serves it).
async fn serve(l: &Ledger) -> SocketAddr {
  let services = Services::new(Database::new(Arc::clone(&l.db), vec![]));
  let app = ledger::router(&l.config, services).expect("the app builds");
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
    .await
    .expect("a port");
  let addr = listener.local_addr().expect("an address");
  tokio::spawn(async move {
    let _ = axum::serve(
      listener,
      app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await;
  });
  addr
}

/// An API token of the user signed in as `who`.
async fn token(l: &Ledger, who: &As) -> String {
  let res = l.post("/auth/tokens", json!({"name": "rpc"}), who).await;
  res.assert_status(StatusCode::CREATED);
  res.json::<Value>().expect("json")["token"]
    .as_str()
    .expect("a token")
    .to_owned()
}

/// The platform HTTP client sending `headers` (a bearer token, an `Idempotency-Key`) with every
/// request.
struct WithHeaders {
  client: ReqwestHttpClient,
  headers: Vec<(String, String)>,
}

impl HttpClient for WithHeaders {
  fn execute(&self, mut req: HttpRequest) -> Effect<HttpResponse, HttpError> {
    req.headers.extend(self.headers.iter().cloned());
    self.client.execute(req)
  }

  fn execute_stream(&self, mut req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
    req.headers.extend(self.headers.iter().cloned());
    self.client.execute_stream(req)
  }
}

/// The Rust client of the Ledger at `addr`, sending `headers`.
fn client(addr: SocketAddr, headers: &[(&str, &str)]) -> LedgerClient<HttpTransport> {
  let config = HttpClientConfig::default()
    // The SSRF guard refuses loopback unless the host is exempt.
    .allow_hosts(["127.0.0.1"])
    .with_timeout(Duration::from_secs(10));
  let http = WithHeaders {
    client: ReqwestHttpClient::from_config(&config),
    headers: headers
      .iter()
      .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
      .collect(),
  };
  LedgerService::client(HttpTransport::new(
    RpcClient::new(format!("http://{addr}")),
    HttpClientService::new(http),
  ))
}

/// The client of the bearer `token`.
fn as_bearer(addr: SocketAddr, token: &str) -> LedgerClient<HttpTransport> {
  client(addr, &[("authorization", &format!("Bearer {token}"))])
}

/// The client of the session `who` signed in with (its cookie; no CSRF token, C5).
fn as_session(addr: SocketAddr, who: &As) -> LedgerClient<HttpTransport> {
  let As::Session(cookie) = who else {
    panic!("not a session: {who:?}")
  };
  client(addr, &[("cookie", cookie.as_str())])
}

/// Runs one client call.
async fn call<A: Send + 'static>(
  effect: Effect<A, RpcCallError<LedgerError>, HNil>,
) -> Result<A, RpcCallError<LedgerError>> {
  run_with_env(HNil, move |_| effect)
    .await
    .into_result()
    .map_err(|cause| cause.fail_option().expect("a typed failure"))
}

/// The status of a call the server refused before it ran (a guard's Problem).
fn refused<A: std::fmt::Debug>(result: Result<A, RpcCallError<LedgerError>>) -> u16 {
  match result {
    Err(RpcCallError::Transport(TransportError::Refused { status, .. })) => status,
    other => panic!("not refused: {other:?}"),
  }
}

/// The typed error of a call.
fn typed<A: std::fmt::Debug>(result: Result<A, RpcCallError<LedgerError>>) -> LedgerError {
  match result {
    Err(RpcCallError::Typed(e)) => e,
    other => panic!("not a typed error: {other:?}"),
  }
}

fn new_account(code: &str, name: &str, kind: &str) -> OpenAccount {
  OpenAccount(NewAccount {
    code: code.to_owned(),
    name: name.to_owned(),
    kind: kind.to_owned(),
  })
}

fn transfer(memo: &str, debit: i64, credit: i64, amount: i64) -> PostEntry {
  PostEntry(NewEntry {
    memo: memo.to_owned(),
    lines: vec![
      Line {
        account_id: debit,
        amount,
      },
      Line {
        account_id: credit,
        amount: -amount,
      },
    ],
  })
}

#[tokio::test]
async fn ledger_rpc_and_rest_agree() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let addr = serve(&l).await;
      let owner = as_bearer(addr, &token(&l, &team.owner).await);
      // A viewer may not mint API tokens: it calls with its session cookie.
      let viewer = as_session(addr, &team.viewer);

      // An account opened over REST, one over RPC: both read back alike on both surfaces.
      let rest = l
        .post(
          "/accounts",
          json!({"code": "1000", "name": "Cash", "kind": "asset"}),
          &team.owner,
        )
        .await;
      rest.assert_status(StatusCode::CREATED);
      let cash: Value = rest.json().unwrap();
      let sales = call(owner.open_account(new_account("4000", "Sales", "income")))
        .await
        .expect("opened over RPC");
      assert_eq!(sales.owner_id, "acme-owner");
      let listed = call(owner.accounts()).await.expect("listed");
      let rest_list: Value = l.get("/accounts", &team.owner).await.json().unwrap();
      assert_eq!(serde_json::to_value(&listed).unwrap(), rest_list);
      let one = call(owner.account(sales.id)).await.unwrap();
      let rest_one: Value = l
        .get(&format!("/accounts/{}", sales.id), &team.owner)
        .await
        .json()
        .unwrap();
      assert_eq!(serde_json::to_value(&one).unwrap(), rest_one);

      // A posting over RPC is the same entry over REST, with the same balances.
      let cash_id = cash["id"].as_i64().unwrap();
      let posted = call(owner.post_entry(transfer("sale", cash_id, sales.id, 700)))
        .await
        .expect("posted");
      assert_eq!(posted.posted_by, "acme-owner");
      let entry = call(owner.entry(posted.id)).await.unwrap();
      let rest_entry: Value = l
        .get(&format!("/entries/{}", posted.id), &team.owner)
        .await
        .json()
        .unwrap();
      assert_eq!(serde_json::to_value(&entry).unwrap(), rest_entry);
      let balance = call(viewer.balance(cash_id)).await.unwrap();
      let rest_balance: Value = l
        .get(&format!("/accounts/{cash_id}/balance"), &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(serde_json::to_value(&balance).unwrap(), rest_balance);
      assert_eq!((balance.balance, balance.postings), (700, 1));
      let page = EntryPage {
        limit: Some(10),
        offset: None,
      };
      let entries = call(owner.entries(page)).await.unwrap();
      let rest_entries: Value = l
        .get("/entries?limit=10", &team.owner)
        .await
        .json()
        .unwrap();
      assert_eq!(serde_json::to_value(&entries).unwrap(), rest_entries);

      // The same failures: what the REST Problem says, as a typed error.
      let missing = l.get("/accounts/999999", &team.owner).await;
      missing.assert_status(StatusCode::NOT_FOUND);
      let detail = missing.json::<Value>().unwrap()["detail"].clone();
      assert_eq!(
        typed(call(owner.account(999_999)).await),
        LedgerError::NotFound {
          detail: detail.as_str().unwrap().to_owned()
        }
      );
      let unknown_account = PostEntry(NewEntry {
        memo: "x".into(),
        lines: vec![
          Line {
            account_id: cash_id,
            amount: 1,
          },
          Line {
            account_id: 999_999,
            amount: -1,
          },
        ],
      });
      match typed(call(owner.post_entry(unknown_account)).await) {
        LedgerError::Invalid { issues, .. } => {
          assert_eq!(issues[0].pointer, "/lines", "{issues:?}");
          assert!(issues[0].message.contains("999999"), "{issues:?}");
        }
        other => panic!("{other:?}"),
      }
      // The input schema is the REST body's: every issue at once, before anything runs.
      match call(owner.open_account(new_account("12", "", "cash"))).await {
        Err(RpcCallError::Failure(failure)) => {
          let text = format!("{failure:?}");
          for field in ["/code", "/name", "/kind"] {
            assert!(text.contains(field), "{text}");
          }
        }
        other => panic!("{other:?}"),
      }
      assert!(matches!(
        typed(call(owner.open_account(new_account("4000", "Again", "income"))).await),
        LedgerError::Conflict { .. }
      ));
      let stale = AccountUpdate {
        id: sales.id,
        name: Some("Revenue".into()),
        locked: None,
        version: 0,
      };
      assert!(matches!(
        typed(call(owner.update_account(stale)).await),
        LedgerError::Conflict { .. }
      ));
      let renamed = call(owner.update_account(AccountUpdate {
        id: sales.id,
        name: Some("Revenue".into()),
        locked: Some(true),
        version: sales.version,
      }))
      .await
      .unwrap();
      assert_eq!((renamed.name.as_str(), renamed.locked), ("Revenue", true));
      let long_name = AccountUpdate {
        id: sales.id,
        name: Some("x".repeat(81)),
        locked: None,
        version: renamed.version,
      };
      assert!(matches!(
        typed(call(owner.update_account(long_name)).await),
        LedgerError::Invalid { .. }
      ));
      for bad in [
        EntryPage {
          limit: Some(0),
          offset: None,
        },
        EntryPage {
          limit: None,
          offset: Some(-1),
        },
      ] {
        assert!(matches!(
          typed(call(owner.entries(bad)).await),
          LedgerError::Invalid { .. }
        ));
      }
      assert!(matches!(
        typed(call(owner.close_account(cash_id)).await),
        LedgerError::Conflict { .. }
      ));

      // The guards of the REST routes: a viewer reads but may not post; nobody signed in is 401.
      assert_eq!(
        refused(call(viewer.post_entry(transfer("no", cash_id, sales.id, 1))).await),
        403
      );
      l.post(
        "/entries",
        json!({"memo": "no", "lines": [
          {"account_id": cash_id, "amount": 1}, {"account_id": sales.id, "amount": -1}]}),
        &team.viewer,
      )
      .await
      .assert_status(StatusCode::FORBIDDEN);
      assert_eq!(refused(call(client(addr, &[]).accounts()).await), 401);
      // Another organisation's account is not found, on both surfaces.
      let other = l.team("globex").await;
      let globex = as_bearer(addr, &token(&l, &other.owner).await);
      assert!(matches!(
        typed(call(globex.account(cash_id)).await),
        LedgerError::NotFound { .. }
      ));

      // A session cookie works too, without a CSRF token (C5: `x-rpc` and JSON).
      let by_cookie = as_session(addr, &team.owner);
      assert_eq!(call(by_cookie.accounts()).await.unwrap().accounts.len(), 2);

      // An empty account closes.
      let spare = call(owner.open_account(new_account("5000", "Spare", "expense")))
        .await
        .unwrap();
      call(owner.close_account(spare.id)).await.expect("closed");
      assert!(matches!(
        typed(call(owner.account(spare.id)).await),
        LedgerError::NotFound { .. }
      ));
    },
  )
  .await;
}

/// The frames of `Ledger.statement` for `account` over HTTP (NDJSON, one frame per line).
async fn statement(l: &Ledger, who: &As, account: i64) -> Vec<Value> {
  let req = l
    .client
    .post_json("/rpc/Ledger.statement", &json!(account))
    .header("x-rpc", "1");
  let res = l.send(l.by(who, req)).await;
  res.assert_status(StatusCode::OK);
  res
    .text()
    .lines()
    .map(|line| serde_json::from_str(line).expect("a frame"))
    .collect()
}

#[tokio::test]
async fn ledger_rpc_statement_streams_every_posting() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-owner", 3).await;
      let (cash, sales, other) = (accounts[0].id, accounts[1].id, accounts[2].id);
      // More postings than one statement page.
      let n = usize::try_from(ledger::reports::STATEMENT_PAGE).unwrap() + 5;
      for i in 1..=n {
        let amount = i64::try_from(i).unwrap();
        l.execute(
          "WITH e AS (INSERT INTO journal_entries (org_id, memo, posted_by) \
             VALUES ('acme', 'bulk', 'acme-owner') RETURNING id) \
           INSERT INTO postings (org_id, entry_id, account_id, amount) \
             SELECT 'acme', e.id, a.account, a.amount FROM e, \
             (VALUES ($1::int8, $3::int8), ($2::int8, -$3::int8)) AS a(account, amount)",
          params![cash, sales, amount],
        )
        .await;
      }
      let frames = statement(&l, &team.viewer, cash).await;
      let items: Vec<&Value> = frames
        .iter()
        .filter(|f| f["kind"] == "item")
        .map(|f| &f["payload"])
        .collect();
      assert_eq!(items.len(), n);
      let mut balance = 0;
      for (i, item) in items.iter().enumerate() {
        balance += i64::try_from(i + 1).unwrap();
        assert_eq!(item["amount"], json!(i + 1));
        assert_eq!(item["balance"], json!(balance));
        assert_eq!(item["memo"], "bulk");
      }
      let exit = frames.last().unwrap();
      assert_eq!(exit["kind"], "exit");
      assert_eq!(exit["payload"]["_tag"], "Success", "{exit}");

      // An account without postings: no item; another organisation's: `NotFound`.
      let frames = statement(&l, &team.viewer, other).await;
      assert_eq!(frames.len(), 1, "{frames:?}");
      assert_eq!(frames[0]["payload"]["_tag"], "Success");
      let globex = l.team("globex").await;
      let frames = statement(&l, &globex.owner, cash).await;
      assert_eq!(frames.len(), 1, "{frames:?}");
      assert_eq!(
        frames[0]["payload"]["error"]["_tag"], "NotFound",
        "{frames:?}"
      );
    },
  )
  .await;
}

#[tokio::test]
async fn ledger_rpc_mutation_idempotent() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-owner", 2).await;
      let (cash, sales) = (accounts[0].id, accounts[1].id);
      let addr = serve(&l).await;
      let owner = token(&l, &team.owner).await;
      let bearer = format!("Bearer {owner}");
      let keyed = |key: &str| {
        client(
          addr,
          &[("authorization", &bearer), ("idempotency-key", key)],
        )
      };
      let entries = || l.scalar("SELECT COUNT(*)::int8 FROM journal_entries", vec![]);

      // A retried posting with the same key posts once and answers the first entry.
      let first = call(keyed("post-0001").post_entry(transfer("rent", cash, sales, 100)))
        .await
        .unwrap();
      let again = call(keyed("post-0001").post_entry(transfer("rent", cash, sales, 100)))
        .await
        .unwrap();
      assert_eq!(again, first);
      assert_eq!(entries().await, 1);
      // On another instance too: the keys live in PostgreSQL.
      let second_instance = serve(&l).await;
      let elsewhere = client(
        second_instance,
        &[("authorization", &bearer), ("idempotency-key", "post-0001")],
      );
      assert_eq!(
        call(elsewhere.post_entry(transfer("rent", cash, sales, 100)))
          .await
          .unwrap(),
        first
      );
      assert_eq!(entries().await, 1);
      // The same key with another payload is refused; a new key posts again.
      assert_eq!(
        refused(call(keyed("post-0001").post_entry(transfer("rent", cash, sales, 101))).await),
        409
      );
      call(keyed("post-0002").post_entry(transfer("rent", cash, sales, 100)))
        .await
        .unwrap();
      assert_eq!(entries().await, 2);
      // Keys are the caller's own: another user's same key is a posting of its own.
      let accountant = token(&l, &team.accountant).await;
      let accountant = format!("Bearer {accountant}");
      let theirs = client(
        addr,
        &[
          ("authorization", &accountant),
          ("idempotency-key", "post-0001"),
        ],
      );
      let other = call(theirs.post_entry(transfer("rent", cash, sales, 100)))
        .await
        .unwrap();
      assert_ne!(other.id, first.id);
      assert_eq!(entries().await, 3);
      // A typed failure is replayed as well: the retry does not run again.
      let broke = call(keyed("post-0003").post_entry(transfer("x", cash, 999_999, 5))).await;
      let replay = call(keyed("post-0003").post_entry(transfer("x", cash, 999_999, 5))).await;
      assert_eq!(typed(replay), typed(broke));
      let stored = l
        .scalar(
          "SELECT COUNT(*)::int8 FROM rpc_idempotency WHERE body IS NOT NULL",
          vec![],
        )
        .await;
      assert_eq!(stored, 4);
    },
  )
  .await;
}

#[tokio::test]
async fn ledger_idempotency_store_reserves_replays_and_expires() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let store = PgIdempotencyStore::new(Arc::clone(&l.db));
      let key = IdempotencyKey::new(
        IdempotencyScope::Principal("acme/ada".into()),
        "Ledger.post_entry@1",
        "k-1",
      );
      let digest = [7; 32];
      assert_eq!(store.begin(&key, digest).await, Begin::Fresh);
      assert_eq!(store.begin(&key, digest).await, Begin::InProgress);
      assert_eq!(store.begin(&key, [8; 32]).await, Begin::Mismatch);
      // Abandoned: the next call runs.
      store.abandon(&key).await;
      assert_eq!(store.begin(&key, digest).await, Begin::Fresh);
      let result = StoredResult {
        digest,
        status: 200,
        body: br#"{"_tag":"Success","value":1}"#.to_vec(),
      };
      store.complete(&key, result.clone()).await;
      assert_eq!(store.begin(&key, digest).await, Begin::Replay(result));
      // A completed key is not abandoned.
      store.abandon(&key).await;
      assert!(matches!(store.begin(&key, digest).await, Begin::Replay(_)));
      // Anonymous callers are scoped by address.
      let anonymous = IdempotencyKey::new(
        IdempotencyScope::Anonymous([10, 0, 0, 1].into()),
        "Ledger.post_entry@1",
        "k-1",
      );
      assert_eq!(store.begin(&anonymous, digest).await, Begin::Fresh);
      // After a day a key is forgotten.
      l.execute(
        "UPDATE rpc_idempotency SET created_at = now() - interval '25 hours' \
         WHERE scope = 'principal:acme/ada'",
        vec![],
      )
      .await;
      assert_eq!(store.begin(&key, digest).await, Begin::Fresh);
      // Without its table the store runs nothing (`InProgress`: the client retries later).
      l.execute("DROP TABLE rpc_idempotency", vec![]).await;
      assert_eq!(store.begin(&key, digest).await, Begin::InProgress);
      let lost = StoredResult {
        digest,
        status: 200,
        body: Vec::new(),
      };
      store.complete(&key, lost).await;
      store.abandon(&key).await;
    },
  )
  .await;
}

/// `deno` on `PATH`, or `None` (a failure under `ID_EFFECT_REQUIRE_DENO=1`).
fn deno() -> Option<PathBuf> {
  let found = std::env::var_os("PATH").and_then(|paths| {
    std::env::split_paths(&paths)
      .map(|dir| dir.join("deno"))
      .find(|p| p.is_file())
  });
  let required = std::env::var("ID_EFFECT_REQUIRE_DENO").is_ok_and(|v| v == "1");
  match found {
    Some(path) => Some(path),
    None if required => panic!("ID_EFFECT_REQUIRE_DENO=1 but `deno` is not on PATH"),
    None => {
      eprintln!("SKIP: `deno` is not on PATH (ID_EFFECT_REQUIRE_DENO=1 fails instead)");
      None
    }
  }
}

#[tokio::test]
async fn ledger_ts_client_round_trip() {
  let Some(deno) = deno() else { return };
  with_ledger(
    |_, _| {},
    move |l| async move {
      let team = l.team("acme").await;
      let addr = serve(&l).await;
      let owner = token(&l, &team.owner).await;
      // A viewer may not mint API tokens: the browser's way, its session cookie.
      let As::Session(viewer) = team.viewer.clone() else {
        unreachable!()
      };
      let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
      let cache = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("deno");
      let url = format!("http://{addr}/rpc");
      let net = format!("--allow-net={addr}");
      let output = tokio::task::spawn_blocking(move || {
        Command::new(deno)
          .args([
            "test",
            "--no-config",
            "--no-remote",
            "--no-prompt",
            &net,
            "tests/ts/ledger.test.ts",
            "--",
            &url,
            &owner,
            &viewer,
          ])
          .current_dir(manifest)
          .env("DENO_DIR", cache)
          .env("NO_COLOR", "1")
          .env("DENO_NO_UPDATE_CHECK", "1")
          .output()
          .expect("deno runs")
      })
      .await
      .expect("deno finished");
      assert!(
        output.status.success(),
        "deno test ({})\n--- stdout\n{}\n--- stderr\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
      );
      // What the TypeScript client posted is in the ledger, once.
      let posted = l
        .scalar(
          "SELECT COUNT(*)::int8 FROM journal_entries WHERE memo = 'a sale'",
          vec![],
        )
        .await;
      assert_eq!(posted, 1);
    },
  )
  .await;
}
