//! The account flows (feature `accounts`): registration, verification, forgot / reset password,
//! over the in-memory stores and the axum routes, and (`live_postgres_*`) over
//! `PgAccountTokenStore` on a live PostgreSQL.
//!
//! `live_postgres_*` tests need a database at `DATABASE_URL`. With `ID_EFFECT_REQUIRE_PG=1` a
//! missing or unreachable database is a test **failure**; without it they print `SKIP` and
//! return.
#![cfg(feature = "accounts")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use http::{Request, StatusCode};
use http_body_util::BodyExt;
use id_effect::{Cause, Effect, Exit, UtcDateTime, WallClock, sleep};
use id_effect_auth::account::{
  AccountError, AccountMail, AccountNotifier, AccountRoutes, AccountTokenStore, Accounts,
  AccountsConfig, MemoryAccountNotifier, MemoryAccountStore, MemoryAccountTokenStore, TokenKind,
};
use id_effect_auth::{
  MemorySessionStore, PasswordHasher, PasswordParams, SessionConfig, Sessions, Verification,
};
use id_effect_tokio::TokioRuntime;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const NEW_PASSWORD: &str = "staple tuna engine";

async fn run<A: Send + 'static>(eff: Effect<A, AccountError>) -> Result<A, AccountError> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn hasher() -> PasswordHasher {
  PasswordHasher::new(PasswordParams::new(8, 1, 1).expect("test params"))
}

fn sha256_hex(s: &str) -> String {
  Sha256::digest(s.as_bytes())
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect()
}

fn secret_of(token: &str) -> &str {
  token.split_once('_').expect("id_secret").1
}

/// The flows over in-memory stores, on an anchored clock tests can move.
struct Fixture {
  accounts: Accounts,
  store: MemoryAccountStore,
  tokens: MemoryAccountTokenStore,
  notifier: MemoryAccountNotifier,
  sessions: Sessions,
  session_store: MemorySessionStore,
  clock: WallClock,
}

fn fixture() -> Fixture {
  let (store, tokens, notifier) = (
    MemoryAccountStore::new(),
    MemoryAccountTokenStore::new(),
    MemoryAccountNotifier::new(),
  );
  let session_store = MemorySessionStore::new();
  let clock = WallClock::anchored(Instant::now(), UtcDateTime::unsafe_make(1_800_000_000_000));
  let sessions =
    Sessions::new(session_store.clone(), SessionConfig::default()).with_clock(clock.clone());
  let accounts = Accounts::new(
    store.clone(),
    tokens.clone(),
    notifier.clone(),
    hasher(),
    sessions.clone(),
  )
  .with_clock(clock.clone());
  Fixture {
    accounts,
    store,
    tokens,
    notifier,
    sessions,
    session_store,
    clock,
  }
}

/// The `n`-th mail of `kind` (1-based), once the background fiber sent it.
async fn mail(notifier: &MemoryAccountNotifier, kind: TokenKind, n: usize) -> AccountMail {
  for _ in 0..400 {
    let of_kind: Vec<AccountMail> = notifier
      .mails()
      .into_iter()
      .filter(|m| m.kind == kind)
      .collect();
    if let Some(m) = of_kind.get(n - 1) {
      return m.clone();
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
  }
  panic!("no mail #{n} of {kind}: {:?}", notifier.mails())
}

/// Registers `email` and redeems its verification mail.
async fn verified(f: &Fixture, email: &str) -> String {
  let account = run(f.accounts.register(email, PASSWORD)).await.unwrap();
  let n = f
    .notifier
    .mails()
    .iter()
    .filter(|m| m.kind == TokenKind::VerifyEmail)
    .count()
    + 1;
  let m = mail(&f.notifier, TokenKind::VerifyEmail, n).await;
  assert_eq!(m.account.id, account.id);
  run(f.accounts.verify_email(m.token.expose()))
    .await
    .unwrap();
  account.id
}

#[tokio::test]
async fn registration_and_verification() {
  let f = fixture();
  let account = run(f.accounts.register(" Carol@Ledger.Example ", PASSWORD))
    .await
    .unwrap();
  assert_eq!(account.email, "carol@ledger.example");
  assert!(!account.email_verified);
  let stored = f.store.password_hash(&account.id).expect("hash stored");
  assert_eq!(
    run(
      hasher()
        .verify(PASSWORD, &stored)
        .map_err(AccountError::from)
    )
    .await,
    Ok(Verification::Valid)
  );

  // Another registration of the address (any case) is refused; so are bad inputs.
  assert_eq!(
    run(f.accounts.register("carol@LEDGER.example", PASSWORD)).await,
    Err(AccountError::EmailTaken)
  );
  assert_eq!(
    run(f.accounts.register("carol", PASSWORD)).await,
    Err(AccountError::InvalidEmail)
  );
  assert!(matches!(
    run(f.accounts.register("dan@ledger.example", "short")).await,
    Err(AccountError::WeakPassword(_))
  ));

  let m = mail(&f.notifier, TokenKind::VerifyEmail, 1).await;
  assert_eq!(m.account.email, "carol@ledger.example");
  assert_eq!(m.valid_for, Duration::from_secs(24 * 3600));
  assert!(!format!("{m:?}").contains(secret_of(m.token.expose())));
  // A verification token is not a reset token.
  assert_eq!(
    run(f.accounts.reset_password(m.token.expose(), NEW_PASSWORD)).await,
    Err(AccountError::InvalidToken)
  );
  let account = run(f.accounts.verify_email(m.token.expose()))
    .await
    .unwrap();
  assert!(account.email_verified);
  assert!(f.store.get(&account.id).unwrap().email_verified);
  assert_eq!(
    run(f.accounts.verify_email(m.token.expose())).await,
    Err(AccountError::InvalidToken)
  );

  // A verified account gets no further verification mail.
  run(f.accounts.request_verification("carol@ledger.example"))
    .await
    .unwrap();
  tokio::time::sleep(Duration::from_millis(50)).await;
  assert_eq!(f.notifier.mails().len(), 1);
  assert!(f.tokens.is_empty());
}

#[tokio::test]
async fn reset_token_is_single_use() {
  let f = fixture();
  let id = verified(&f, "erin@ledger.example").await;
  run(f.accounts.forgot_password("erin@ledger.example"))
    .await
    .unwrap();
  let m = mail(&f.notifier, TokenKind::ResetPassword, 1).await;
  assert_eq!(m.account.id, id);
  let token = m.token.expose().to_owned();

  // Two requests with the same token at once: exactly one resets.
  let (a, b) = tokio::join!(
    run(f.accounts.reset_password(&token, NEW_PASSWORD)),
    run(f.accounts.reset_password(&token, "another passphrase"))
  );
  let outcomes = [a.is_ok(), b.is_ok()];
  assert_eq!(outcomes.iter().filter(|ok| **ok).count(), 1, "{a:?} {b:?}");
  let lost = if a.is_ok() { b } else { a };
  assert_eq!(lost, Err(AccountError::InvalidToken));

  // And never again.
  assert_eq!(
    run(f.accounts.reset_password(&token, "yet another passphrase")).await,
    Err(AccountError::InvalidToken)
  );
  assert!(f.tokens.is_empty());

  // A wrong secret under a real id is rejected and does not burn the token.
  run(f.accounts.forgot_password("erin@ledger.example"))
    .await
    .unwrap();
  let m = mail(&f.notifier, TokenKind::ResetPassword, 2).await;
  let (tid, _) = m.token.expose().split_once('_').unwrap();
  let forged = format!("{tid}_{}", "0".repeat(64));
  assert_eq!(
    run(f.accounts.reset_password(&forged, NEW_PASSWORD)).await,
    Err(AccountError::InvalidToken)
  );
  for bad in ["", "nonsense", &format!("{tid}_")] {
    assert_eq!(
      run(f.accounts.reset_password(bad, NEW_PASSWORD)).await,
      Err(AccountError::InvalidToken)
    );
  }
  run(f.accounts.reset_password(m.token.expose(), NEW_PASSWORD))
    .await
    .unwrap();
}

#[tokio::test]
async fn expired_token_rejected() {
  let f = fixture();
  let id = verified(&f, "frank@ledger.example").await;
  run(f.accounts.forgot_password("frank@ledger.example"))
    .await
    .unwrap();
  let m = mail(&f.notifier, TokenKind::ResetPassword, 1).await;
  assert_eq!(
    m.valid_for,
    Duration::from_secs(30 * 60),
    "30 minutes by default"
  );
  let before = f.store.password_hash(&id);

  f.clock.jump(Duration::from_secs(30 * 60));
  assert_eq!(
    run(f.accounts.reset_password(m.token.expose(), NEW_PASSWORD)).await,
    Err(AccountError::ExpiredToken)
  );
  assert!(f.tokens.is_empty(), "an expired token is removed");
  assert_eq!(
    f.store.password_hash(&id),
    before,
    "the password is unchanged"
  );
  assert_eq!(
    run(f.accounts.reset_password(m.token.expose(), NEW_PASSWORD)).await,
    Err(AccountError::InvalidToken)
  );

  // TTLs are configurable; verification tokens expire too.
  let f = fixture();
  let accounts = f.accounts.clone().with_config(AccountsConfig {
    verify_ttl: Duration::from_secs(60),
    ..AccountsConfig::default()
  });
  run(accounts.register("gail@ledger.example", PASSWORD))
    .await
    .unwrap();
  let m = mail(&f.notifier, TokenKind::VerifyEmail, 1).await;
  assert_eq!(m.valid_for, Duration::from_secs(60));
  f.clock.jump(Duration::from_secs(59));
  run(f.accounts.purge_expired_tokens()).await.unwrap();
  assert_eq!(f.tokens.len(), 1, "still live");
  f.clock.jump(Duration::from_secs(1));
  assert_eq!(
    run(accounts.verify_email(m.token.expose())).await,
    Err(AccountError::ExpiredToken)
  );
}

#[tokio::test]
async fn token_stored_hashed() {
  let f = fixture();
  let id = verified(&f, "hana@ledger.example").await;
  run(f.accounts.forgot_password("hana@ledger.example"))
    .await
    .unwrap();
  let first = mail(&f.notifier, TokenKind::ResetPassword, 1).await;
  run(f.accounts.forgot_password("hana@ledger.example"))
    .await
    .unwrap();
  let second = mail(&f.notifier, TokenKind::ResetPassword, 2).await;

  // A new token replaces the earlier one; what is stored is the id and the digest.
  let records = f.tokens.records();
  assert_eq!(records.len(), 1);
  let token = second.token.expose();
  let (tid, secret) = token.split_once('_').unwrap();
  let r = &records[0];
  assert_eq!(
    (r.id(), r.account_id(), r.kind()),
    (tid, id.as_str(), TokenKind::ResetPassword)
  );
  assert_eq!(r.digest_hex(), sha256_hex(secret));
  let dump = format!("{records:?} {}", r.digest_hex());
  assert!(!dump.contains(secret) && !dump.contains(token));
  assert_eq!(
    run(
      f.accounts
        .reset_password(first.token.expose(), NEW_PASSWORD)
    )
    .await,
    Err(AccountError::InvalidToken),
    "the replaced token is dead"
  );
  run(f.accounts.reset_password(token, NEW_PASSWORD))
    .await
    .unwrap();
}

#[tokio::test]
async fn reset_revokes_sessions() {
  let f = fixture();
  let id = verified(&f, "ivan@ledger.example").await;
  let other = verified(&f, "judy@ledger.example").await;
  let s1 = f.sessions.login(id.clone(), None);
  let s1 = run(s1.map_err(AccountError::from)).await.unwrap();
  let s2 = run(
    f.sessions
      .login(id.clone(), None)
      .map_err(AccountError::from),
  )
  .await
  .unwrap();
  let s3 = run(
    f.sessions
      .login(other.clone(), None)
      .map_err(AccountError::from),
  )
  .await
  .unwrap();
  assert_eq!(f.session_store.len(), 3);

  run(f.accounts.forgot_password("ivan@ledger.example"))
    .await
    .unwrap();
  let m = mail(&f.notifier, TokenKind::ResetPassword, 1).await;
  let account = run(f.accounts.reset_password(m.token.expose(), NEW_PASSWORD))
    .await
    .unwrap();
  assert_eq!(account.id, id);

  let load = |s: &id_effect_auth::Session| f.sessions.load(s.id()).map_err(AccountError::from);
  assert_eq!(run(load(&s1)).await, Ok(None));
  assert_eq!(run(load(&s2)).await, Ok(None));
  assert!(
    run(load(&s3)).await.unwrap().is_some(),
    "other accounts keep theirs"
  );

  // The new password was re-hashed through the PasswordHasher; the old one no longer verifies.
  let stored = f.store.password_hash(&id).unwrap();
  let check = |pw: &str| hasher().verify(pw, &stored).map_err(AccountError::from);
  assert_eq!(run(check(NEW_PASSWORD)).await, Ok(Verification::Valid));
  assert_eq!(run(check(PASSWORD)).await, Ok(Verification::Invalid));
  assert!(matches!(
    run(f.accounts.reset_password(m.token.expose(), "short")).await,
    Err(AccountError::WeakPassword(_))
  ));
}

/// A notifier that never finishes: the mail transport hangs.
#[derive(Clone, Default)]
struct HangingNotifier {
  calls: Arc<Mutex<Vec<String>>>,
}

impl AccountNotifier for HangingNotifier {
  fn notify(&self, mail: AccountMail) -> Effect<(), AccountError> {
    self.calls.lock().unwrap().push(mail.account.email);
    sleep(Duration::from_secs(3600))
  }
}

async fn post(app: &Router, path: &str, body: &str) -> (StatusCode, Vec<(String, String)>, String) {
  let request = Request::post(path)
    .header("content-type", "application/json")
    .body(Body::from(body.to_owned()))
    .unwrap();
  let response = app.clone().oneshot(request).await.unwrap();
  let status = response.status();
  let mut headers: Vec<(String, String)> = response
    .headers()
    .iter()
    .filter(|(name, _)| *name != "content-length")
    .map(|(n, v)| (n.to_string(), v.to_str().unwrap().to_owned()))
    .collect();
  headers.sort();
  let bytes = response.into_body().collect().await.unwrap().to_bytes();
  (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn forgot_password_response_is_uniform() {
  let f = fixture();
  verified(&f, "kim@ledger.example").await;
  let app: Router = AccountRoutes::new(f.accounts.clone()).router();

  let known = post(
    &app,
    "/auth/password/forgot",
    r#"{"email":"kim@ledger.example"}"#,
  )
  .await;
  let cased = post(
    &app,
    "/auth/password/forgot",
    r#"{"email":"KIM@ledger.example"}"#,
  )
  .await;
  let unknown = post(
    &app,
    "/auth/password/forgot",
    r#"{"email":"nobody@ledger.example"}"#,
  )
  .await;
  assert_eq!(known.0, StatusCode::ACCEPTED);
  assert_eq!(known, unknown, "status, headers and body are the same");
  assert_eq!(known, cased);
  assert!(
    known
      .1
      .contains(&("cache-control".into(), "no-store".into()))
  );
  mail(&f.notifier, TokenKind::ResetPassword, 2).await;
  tokio::time::sleep(Duration::from_millis(50)).await;
  assert_eq!(
    f.notifier
      .mails()
      .iter()
      .filter(|m| m.kind == TokenKind::ResetPassword)
      .count(),
    2,
    "only the known address got mail"
  );

  // The response never waits on the mail transport: with a transport that hangs, the known
  // address answers as fast as the unknown one.
  let hanging = HangingNotifier::default();
  let store = MemoryAccountStore::new();
  let slow = Accounts::new(
    store.clone(),
    MemoryAccountTokenStore::new(),
    hanging.clone(),
    hasher(),
    f.sessions.clone(),
  );
  run(slow.register("lee@ledger.example", PASSWORD))
    .await
    .unwrap();
  let app: Router = AccountRoutes::new(slow).router();
  let known = tokio::time::timeout(
    Duration::from_secs(5),
    post(
      &app,
      "/auth/password/forgot",
      r#"{"email":"lee@ledger.example"}"#,
    ),
  )
  .await
  .expect("the response does not wait for the mail");
  let unknown = post(
    &app,
    "/auth/password/forgot",
    r#"{"email":"may@ledger.example"}"#,
  )
  .await;
  assert_eq!(known, unknown);
  for _ in 0..400 {
    if hanging.calls.lock().unwrap().len() == 2 {
      break;
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
  }
  assert_eq!(
    *hanging.calls.lock().unwrap(),
    ["lee@ledger.example", "lee@ledger.example"],
    "registration and forgot-password mails were started in the background"
  );

  // The verification resend answers the same way.
  let app: Router = AccountRoutes::new(f.accounts.clone()).router();
  let a = post(
    &app,
    "/auth/verify/resend",
    r#"{"email":"kim@ledger.example"}"#,
  )
  .await;
  let b = post(
    &app,
    "/auth/verify/resend",
    r#"{"email":"nobody@ledger.example"}"#,
  )
  .await;
  assert_eq!(a, b);
  assert_eq!(a.0, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn the_routes_run_the_flows() {
  let f = fixture();
  let app: Router = AccountRoutes::new(f.accounts.clone())
    .base_path("/account")
    .router();

  let (status, _, body) = post(
    &app,
    "/account/register",
    &format!(r#"{{"email":"nia@ledger.example","password":"{PASSWORD}"}}"#),
  )
  .await;
  assert_eq!(status, StatusCode::CREATED, "{body}");
  let created: serde_json::Value = serde_json::from_str(&body).unwrap();
  assert_eq!(created["email"], "nia@ledger.example");
  assert_eq!(created["email_verified"], false);
  let again = post(
    &app,
    "/account/register",
    &format!(r#"{{"email":"nia@ledger.example","password":"{PASSWORD}"}}"#),
  )
  .await;
  assert_eq!(again.0, StatusCode::CONFLICT);
  let weak = post(
    &app,
    "/account/register",
    r#"{"email":"oz@ledger.example","password":"short"}"#,
  )
  .await;
  assert_eq!(weak.0, StatusCode::UNPROCESSABLE_ENTITY);
  assert_eq!(
    post(&app, "/account/register", "{").await.0,
    StatusCode::BAD_REQUEST
  );

  let m = mail(&f.notifier, TokenKind::VerifyEmail, 1).await;
  let token = m.token.expose();
  let (status, _, body) = post(
    &app,
    "/account/verify",
    &format!(r#"{{"token":"{token}"}}"#),
  )
  .await;
  assert_eq!(status, StatusCode::OK, "{body}");
  assert!(body.contains(r#""email_verified":true"#));
  let reused = post(
    &app,
    "/account/verify",
    &format!(r#"{{"token":"{token}"}}"#),
  )
  .await;
  assert_eq!(reused.0, StatusCode::BAD_REQUEST);

  post(
    &app,
    "/account/password/forgot",
    r#"{"email":"nia@ledger.example"}"#,
  )
  .await;
  let m = mail(&f.notifier, TokenKind::ResetPassword, 1).await;
  let token = m.token.expose();
  let reset = format!(r#"{{"token":"{token}","password":"{NEW_PASSWORD}"}}"#);
  assert_eq!(
    post(&app, "/account/password/reset", &reset).await.0,
    StatusCode::NO_CONTENT
  );
  assert_eq!(
    post(&app, "/account/password/reset", &reset).await.0,
    StatusCode::BAD_REQUEST
  );
  let bad = post(
    &app,
    "/account/password/forgot",
    r#"{"email":"not an address"}"#,
  )
  .await;
  assert_eq!(bad.0, StatusCode::UNPROCESSABLE_ENTITY);

  // A store outage answers 503 without describing it.
  f.store.fail_with(Some("db password=hunter2"));
  let (status, _, body) = post(
    &app,
    "/account/password/forgot",
    r#"{"email":"nia@ledger.example"}"#,
  )
  .await;
  assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
  assert!(!body.contains("hunter2"), "{body}");
}

#[tokio::test]
async fn background_failures_reach_the_hook() {
  let f = fixture();
  let seen = Arc::new(Mutex::new(Vec::new()));
  let sink = Arc::clone(&seen);
  let accounts = f
    .accounts
    .clone()
    .on_background_error(move |e| sink.lock().unwrap().push(e.clone()));
  f.notifier.fail_with(Some("smtp down"));
  // Registration still succeeds: the mail is background work, and the user can ask again.
  run(accounts.register("pat@ledger.example", PASSWORD))
    .await
    .unwrap();
  for _ in 0..400 {
    if !seen.lock().unwrap().is_empty() {
      break;
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
  }
  assert_eq!(
    *seen.lock().unwrap(),
    [AccountError::Notify("smtp down".into())]
  );
  f.notifier.fail_with(None);
  run(accounts.request_verification("pat@ledger.example"))
    .await
    .unwrap();
  let m = mail(&f.notifier, TokenKind::VerifyEmail, 1).await;
  run(accounts.verify_email(m.token.expose())).await.unwrap();
  assert!(format!("{accounts:?}").contains("Accounts"));
}

#[cfg(feature = "pg")]
mod pg {
  use id_effect_auth::PgAccountTokenStore;
  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
  use id_effect_sql::{SqlClient, SqlError, SqlValue};

  use super::*;

  const DOWN: &str = include_str!("../migrations/0002_id_effect_account_tokens.down.sql");

  async fn sql<A: Send + 'static>(eff: Effect<A, SqlError>) -> A {
    match TokioRuntime::current().expect("tokio").run(eff).await {
      Exit::Success(a) => a,
      Exit::Failure(c) => panic!("statement failed: {c:?}"),
    }
  }

  fn pg_required() -> bool {
    std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
  }

  fn unavailable<T>(test: &str, why: &str) -> Option<T> {
    assert!(
      !pg_required(),
      "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
    );
    eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
    None
  }

  async fn live_client(test: &str) -> Option<PgSqlClient> {
    let url = match std::env::var("DATABASE_URL") {
      Ok(url) => url,
      Err(_) => return unavailable(test, "DATABASE_URL is not set"),
    };
    let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(4)).await {
      Ok(p) => p,
      Err(e) => return unavailable(test, &format!("cannot build pool: {}", e.0)),
    };
    let client = PgSqlClient::new(pool);
    if let Err(e) = run(
      client
        .connect()
        .map_err(|e| AccountError::Storage(e.to_string())),
    )
    .await
    {
      return unavailable(test, &format!("cannot connect: {e:?}"));
    }
    Some(client)
  }

  async fn texts(client: &PgSqlClient, query: &str, params: &[SqlValue]) -> Vec<String> {
    sql(client.query(query, params))
      .await
      .iter()
      .map(|r| r.get::<String>(0usize).expect("text column"))
      .collect()
  }

  /// One test, so the schema checks, the flows and the down migration never interleave on the
  /// shared table.
  #[tokio::test]
  async fn live_postgres_account_tokens() {
    let test = "live_postgres_account_tokens";
    let Some(client) = live_client(test).await else {
      return;
    };
    let tokens = PgAccountTokenStore::new(client.clone());
    sql(client.execute(DOWN.trim(), &[])).await;
    run(tokens.ensure_schema()).await.unwrap();
    run(tokens.ensure_schema()).await.unwrap();
    let columns = texts(
      &client,
      "SELECT column_name || ':' || data_type FROM information_schema.columns \
       WHERE table_name = 'id_effect_account_tokens' ORDER BY ordinal_position",
      &[],
    )
    .await;
    assert_eq!(
      columns,
      [
        "id:text",
        "kind:text",
        "account_id:text",
        "secret_hash:text",
        "expires_at:timestamp with time zone",
      ]
    );

    let f = fixture();
    let notifier = f.notifier.clone();
    let accounts = Accounts::new(
      f.store.clone(),
      tokens.clone(),
      notifier.clone(),
      hasher(),
      f.sessions.clone(),
    )
    .with_clock(f.clock.clone());
    let account = run(accounts.register("quinn@ledger.example", PASSWORD))
      .await
      .unwrap();
    let verify = mail(&notifier, TokenKind::VerifyEmail, 1).await;
    run(accounts.forgot_password("quinn@ledger.example"))
      .await
      .unwrap();
    let reset = mail(&notifier, TokenKind::ResetPassword, 1).await;

    // token_stored_hashed, in the table: the digest of the secret, the token nowhere.
    for m in [&verify, &reset] {
      let token = m.token.expose();
      let (tid, secret) = token.split_once('_').unwrap();
      let stored = texts(
        &client,
        "SELECT secret_hash FROM id_effect_account_tokens WHERE id = $1",
        &[SqlValue::Text(tid.into())],
      )
      .await;
      assert_eq!(stored, [sha256_hex(secret)]);
      let leaked = texts(
        &client,
        "SELECT id FROM id_effect_account_tokens t WHERE strpos(t::text, $1) > 0",
        &[SqlValue::Text(secret.into())],
      )
      .await;
      assert!(leaked.is_empty(), "the secret is in a row");
    }

    // Single use and expiry against the database.
    run(accounts.reset_password(reset.token.expose(), NEW_PASSWORD))
      .await
      .unwrap();
    assert_eq!(
      run(accounts.reset_password(reset.token.expose(), NEW_PASSWORD)).await,
      Err(AccountError::InvalidToken)
    );
    f.clock.jump(Duration::from_secs(24 * 3600));
    assert_eq!(
      run(accounts.verify_email(verify.token.expose())).await,
      Err(AccountError::ExpiredToken)
    );
    assert!(
      texts(&client, "SELECT id FROM id_effect_account_tokens", &[])
        .await
        .is_empty()
    );
    let (_, record) = id_effect_auth::AccountToken::generate(
      TokenKind::VerifyEmail,
      account.id.clone(),
      UtcDateTime::unsafe_make(1_000),
    )
    .unwrap();
    run(tokens.insert(record)).await.unwrap();
    assert_eq!(run(accounts.purge_expired_tokens()).await, Ok(1));
  }
}
