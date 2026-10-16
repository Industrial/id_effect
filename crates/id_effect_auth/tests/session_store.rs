//! The `SessionStore` behaviour suite, run against `MemorySessionStore` and (feature `pg`)
//! `PgSessionStore` on a live PostgreSQL.
//!
//! `live_postgres_*` tests need a database at `DATABASE_URL`. When CI provides a Postgres service
//! it also sets `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**. Without it, those tests print an explicit `SKIP` line to stderr and return.
//!
//! Effects run on `id_effect_tokio::TokioRuntime` (sqlx needs tokio's IO driver).

use std::collections::BTreeMap;

use id_effect::{Cause, Effect, Exit, UtcDateTime};
use id_effect_auth::{MemorySessionStore, Session, SessionError, SessionId, SessionStore};
use id_effect_tokio::TokioRuntime;

async fn run<A: Send + 'static>(eff: Effect<A, SessionError>) -> Result<A, SessionError> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// `T + secs` seconds, at whole seconds (databases keep microseconds).
fn at(secs: i64) -> UtcDateTime {
  UtcDateTime::unsafe_make((1_800_000_000 + secs) * 1000)
}

fn session(user: &str, attrs: &[(&str, &str)], idle: i64, absolute: i64) -> Session {
  let attrs: BTreeMap<String, String> = attrs
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
  Session::from_parts(
    SessionId::generate().unwrap(),
    user,
    attrs,
    at(-100),
    at(-50),
    at(idle),
    at(absolute),
  )
}

/// What every `SessionStore` does. `store` must hold no record of the users `alice`, `bob`,
/// `carol`, `dave` and no record that is expired at `at(0)`; the suite leaves it that way.
async fn store_behaviour(store: &impl SessionStore) {
  // Round trip, attributes included.
  let s = session(
    "alice",
    &[("tenant", "acme"), ("note", "ü ✓ \"q\"")],
    600,
    3600,
  );
  run(store.save(s.clone())).await.unwrap();
  assert_eq!(run(store.load(s.id())).await, Ok(Some(s.clone())));

  // `save` replaces the record with the same id (an upsert, never a second record).
  let mut changed = Session::from_parts(
    s.id().clone(),
    "alice",
    BTreeMap::new(),
    s.created_at(),
    at(10),
    at(610),
    s.absolute_expires_at(),
  );
  changed.set_attr("role", "admin");
  run(store.save(changed.clone())).await.unwrap();
  assert_eq!(run(store.load(s.id())).await, Ok(Some(changed.clone())));

  // Unknown ids.
  let unknown = SessionId::generate().unwrap();
  assert_eq!(run(store.load(&unknown)).await, Ok(None));
  assert_eq!(run(store.remove(&unknown)).await, Ok(false));

  // `remove` is true once.
  assert_eq!(run(store.remove(s.id())).await, Ok(true));
  assert_eq!(run(store.remove(s.id())).await, Ok(false));
  assert_eq!(run(store.load(s.id())).await, Ok(None));

  // `remove_user` drops every record of that user and only those.
  let (b1, b2) = (
    session("bob", &[], 600, 3600),
    session("bob", &[], 600, 3600),
  );
  let c1 = session("carol", &[], 600, 3600);
  for s in [&b1, &b2, &c1] {
    run(store.save(s.clone())).await.unwrap();
  }
  run(store.save(b1.clone())).await.unwrap();
  assert_eq!(run(store.remove_user("bob")).await, Ok(2));
  assert_eq!(run(store.remove_user("bob")).await, Ok(0));
  assert_eq!(run(store.load(b2.id())).await, Ok(None));
  assert_eq!(run(store.load(c1.id())).await, Ok(Some(c1.clone())));

  // `purge_expired(now)` drops records whose idle or absolute expiry is at or before `now`.
  let idle_gone = session("dave", &[], -1, 3600);
  let absolute_gone = session("dave", &[], 600, 0);
  let live = session("dave", &[], 1, 3600);
  for s in [&idle_gone, &absolute_gone, &live] {
    run(store.save(s.clone())).await.unwrap();
  }
  assert_eq!(run(store.purge_expired(at(0))).await, Ok(2));
  assert_eq!(run(store.purge_expired(at(0))).await, Ok(0));
  assert_eq!(run(store.load(idle_gone.id())).await, Ok(None));
  assert_eq!(run(store.load(absolute_gone.id())).await, Ok(None));
  assert_eq!(run(store.load(live.id())).await, Ok(Some(live.clone())));
  assert_eq!(run(store.load(c1.id())).await, Ok(Some(c1)));

  // Leave the store as it was.
  assert_eq!(run(store.remove_user("carol")).await, Ok(1));
  assert_eq!(run(store.remove_user("dave")).await, Ok(1));
}

#[tokio::test]
async fn memory_store_passes_the_suite() {
  let store = MemorySessionStore::new();
  store_behaviour(&store).await;
  assert!(store.is_empty());
}

#[cfg(feature = "pg")]
mod pg {
  use std::time::Duration;

  use id_effect_auth::{PgSessionStore, SessionConfig, Sessions};
  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
  use id_effect_sql::{SqlClient, SqlError, SqlValue};
  use sha2::{Digest, Sha256};

  use super::*;

  const DOWN: &str = include_str!("../migrations/0001_id_effect_sessions.down.sql");

  async fn sql<A: Send + 'static>(eff: Effect<A, SqlError>) -> A {
    match TokioRuntime::current().expect("tokio").run(eff).await {
      Exit::Success(a) => a,
      Exit::Failure(c) => panic!("statement failed: {c:?}"),
    }
  }

  fn pg_required() -> bool {
    std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
  }

  /// Fail (when `ID_EFFECT_REQUIRE_PG=1`) or loudly skip a live test that has no database.
  fn unavailable<T>(test: &str, why: &str) -> Option<T> {
    assert!(
      !pg_required(),
      "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
    );
    eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
    None
  }

  /// Connected client for a live test, or `None` after [`unavailable`] decided to skip.
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
        .map_err(|e| SessionError::Storage(e.to_string())),
    )
    .await
    {
      return unavailable(test, &format!("cannot connect: {e:?}"));
    }
    Some(client)
  }

  async fn scalar_text(client: &PgSqlClient, query: &str, params: &[SqlValue]) -> Vec<String> {
    sql(client.query(query, params))
      .await
      .iter()
      .map(|r| r.get::<String>(0usize).expect("text column"))
      .collect()
  }

  /// One test, so the suite, the schema checks and the down migration never interleave on the
  /// shared table.
  #[tokio::test]
  async fn live_postgres_session_store() {
    let test = "live_postgres_session_store";
    let Some(client) = live_client(test).await else {
      return;
    };
    let store = PgSessionStore::new(client.clone());

    // The down migration drops the table; `ensure_schema` (re)creates it and is idempotent.
    sql(client.execute(DOWN.trim(), &[])).await;
    run(store.ensure_schema()).await.unwrap();
    run(store.ensure_schema()).await.unwrap();

    let columns = scalar_text(
      &client,
      "SELECT column_name || ':' || data_type FROM information_schema.columns \
       WHERE table_name = 'id_effect_sessions' ORDER BY ordinal_position",
      &[],
    )
    .await;
    assert_eq!(
      columns,
      [
        "id_hash:text",
        "user_id:text",
        "attrs:jsonb",
        "created_at:timestamp with time zone",
        "last_seen_at:timestamp with time zone",
        "idle_expires_at:timestamp with time zone",
        "absolute_expires_at:timestamp with time zone",
      ]
    );
    let indexes = scalar_text(
      &client,
      "SELECT indexname::text FROM pg_indexes WHERE tablename = 'id_effect_sessions' \
       ORDER BY indexname",
      &[],
    )
    .await;
    assert_eq!(
      indexes,
      [
        "id_effect_sessions_absolute_expires_at_idx",
        "id_effect_sessions_pkey",
        "id_effect_sessions_user_id_idx",
      ]
    );

    // The shared behaviour suite.
    store_behaviour(&store).await;
    assert!(
      scalar_text(&client, "SELECT id_hash FROM id_effect_sessions", &[])
        .await
        .is_empty()
    );

    // The key is the SHA-256 of the id; the id itself is stored nowhere.
    let s = session("erin", &[("k", "v")], 600, 3600);
    run(store.save(s.clone())).await.unwrap();
    let expected: String = Sha256::digest(s.id().as_str().as_bytes())
      .iter()
      .map(|b| format!("{b:02x}"))
      .collect();
    assert_eq!(
      scalar_text(&client, "SELECT id_hash FROM id_effect_sessions", &[]).await,
      [expected]
    );
    let leaked = scalar_text(
      &client,
      "SELECT id_hash FROM id_effect_sessions t WHERE strpos(t::text, $1) > 0",
      &[SqlValue::Text(s.id().as_str().into())],
    )
    .await;
    assert!(leaked.is_empty(), "the raw session id is in a row");
    assert_eq!(run(store.remove_user("erin")).await, Ok(1));

    // `Sessions` over the store, on the system clock.
    let sessions = Sessions::new(
      store.clone(),
      SessionConfig {
        touch_interval: Duration::ZERO,
        ..SessionConfig::default()
      },
    );
    let mut login = run(sessions.login("frank", None)).await.unwrap();
    login.set_attr("tenant", "acme");
    run(sessions.save(login.clone())).await.unwrap();
    let loaded = run(sessions.load(login.id())).await.unwrap().expect("live");
    assert_eq!(loaded.attr("tenant"), Some("acme"));
    assert_eq!(loaded.user_id(), "frank");
    let rotated = run(sessions.rotate(login.id()))
      .await
      .unwrap()
      .expect("live");
    assert_eq!(run(sessions.load(login.id())).await, Ok(None));
    assert_eq!(run(sessions.revoke_all("frank")).await, Ok(1));
    assert_eq!(run(sessions.load(rotated.id())).await, Ok(None));
  }
}
