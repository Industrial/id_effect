//! `RedisSessionStore` against a Redis container: id_effect_auth's `SessionStore` behaviour
//! suite (the one `MemorySessionStore` and `PgSessionStore` pass, in
//! `crates/id_effect_auth/tests/session_store.rs`), then what is specific to Redis.

#![cfg(feature = "session")]

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::{ok, redis_client};
use id_effect::{Cause, Effect, Exit, UtcDateTime, need};
use id_effect_auth::session::{SessionConfig, SessionStoreEnv, SessionStoreService, Sessions};
use id_effect_auth::{Session, SessionError, SessionId, SessionStore};
use id_effect_redis::{
  RedisClient, RedisConfig, RedisEnv, RedisError, RedisSessionStore, redis_session_store_layer,
};
use id_effect_tokio::TokioRuntime;
use sha2::{Digest, Sha256};

async fn run<A: Send + 'static>(eff: Effect<A, SessionError>) -> Result<A, SessionError> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// `T + secs` seconds, at whole seconds.
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

/// The `SessionStore` behaviour suite of id_effect_auth, verbatim.
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

  // Leave the store as it was.
  assert_eq!(run(store.remove_user("carol")).await, Ok(1));
  assert_eq!(run(store.remove_user("dave")).await, Ok(1));
}

fn sha256_hex(s: &str) -> String {
  Sha256::digest(s.as_bytes())
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redis_session_store() {
  let Some((_redis, client)) = redis_client("redis_session_store").await else {
    return;
  };
  let store = RedisSessionStore::new(client.clone());

  // The shared behaviour suite leaves no key behind.
  store_behaviour(&store).await;
  assert_eq!(ok(client.query::<u64>(redis::cmd("DBSIZE"))).await, 0);

  // The key is the SHA-256 of the id; the id itself is stored nowhere; the record expires with
  // the session; a user switch moves the record between user indexes.
  let s = session("erin", &[("k", "v")], 600, 3600);
  run(store.save(s.clone())).await.unwrap();
  let record = format!("id_effect:session:{}", sha256_hex(s.id().as_str()));
  let keys = ok(client.query::<Vec<String>>(redis::cmd("KEYS").arg("*").clone())).await;
  assert!(keys.contains(&record), "{keys:?}");
  for key in &keys {
    assert!(!key.contains(s.id().as_str()));
    let dump = ok(client.query::<redis::Value>(redis::cmd("DUMP").arg(key).clone())).await;
    assert!(!format!("{dump:?}").contains(s.id().as_str()));
  }
  let expire_at = ok(client.query::<i64>(redis::cmd("PEXPIRETIME").arg(&record).clone())).await;
  assert_eq!(expire_at, (1_800_000_000 + 600) * 1000);
  let moved = Session::from_parts(
    s.id().clone(),
    "frank",
    BTreeMap::new(),
    s.created_at(),
    s.last_seen_at(),
    s.idle_expires_at(),
    s.absolute_expires_at(),
  );
  run(store.save(moved.clone())).await.unwrap();
  assert_eq!(run(store.remove_user("erin")).await, Ok(0));
  assert_eq!(run(store.load(s.id())).await, Ok(Some(moved)));
  assert_eq!(run(store.remove_user("frank")).await, Ok(1));

  // Sub-second times round-trip exactly.
  let precise = Session::from_parts(
    SessionId::generate().unwrap(),
    "gus",
    BTreeMap::new(),
    UtcDateTime(jiff::Timestamp::from_nanosecond(1_800_000_000_123_456_789).unwrap()),
    at(1),
    at(600),
    at(3600),
  );
  run(store.save(precise.clone())).await.unwrap();
  assert_eq!(
    run(store.load(precise.id())).await,
    Ok(Some(precise.clone()))
  );

  // A record Redis cannot decode is a storage error.
  let broken = format!("id_effect:session:{}", sha256_hex(precise.id().as_str()));
  ok(
    client.query::<()>(
      redis::cmd("HSET")
        .arg(&broken)
        .arg("attrs")
        .arg("[")
        .clone(),
    ),
  )
  .await;
  assert!(matches!(
    run(store.load(precise.id())).await,
    Err(SessionError::Storage(m)) if m.contains("bad attrs")
  ));
  assert_eq!(run(store.remove(precise.id())).await, Ok(true));

  // A prefix keeps applications apart.
  let other = RedisSessionStore::new(client.clone()).with_prefix("other");
  let t = session("hal", &[], 600, 3600);
  run(other.save(t.clone())).await.unwrap();
  assert_eq!(run(store.load(t.id())).await, Ok(None));
  assert_eq!(run(other.load(t.id())).await, Ok(Some(t)));
  assert!(format!("{other:?}").contains("other"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_run_on_the_redis_store_layer() {
  let Some((_redis, client)) = redis_client("sessions_run_on_the_redis_store_layer").await else {
    return;
  };
  let url = client.config().url().to_owned();
  let layer = RedisClient::layer::<SessionError>(RedisConfig::new(url))
    .provide_to(redis_session_store_layer());
  let program: Effect<Option<String>, SessionError, SessionStoreEnv> =
    need::<SessionStoreService>().flat_map(|store| {
      let sessions = Sessions::from_service((*store).clone(), SessionConfig::default());
      let again = sessions.clone();
      sessions
        .login("user-42", None)
        .flat_map(move |s| again.load(s.id()))
        .map(|s| s.map(|s| s.user_id().to_owned()))
        .narrow()
    });
  assert_eq!(
    run(program.provide_layer(layer)).await,
    Ok(Some("user-42".to_owned()))
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_server_is_a_storage_error() {
  let config = RedisConfig::new("redis://127.0.0.1:1")
    .pool_size(1)
    .reconnect_retries(0)
    .connection_timeout(Duration::from_millis(200));
  let client = ok(RedisClient::connect_lazy(config.clone())).await;
  let store = RedisSessionStore::new(client);
  let s = session("x", &[], 1, 2);
  let down =
    |r: Result<(), SessionError>| matches!(r, Err(SessionError::Storage(m)) if m.contains("redis"));
  assert!(down(run(store.save(s.clone())).await));
  assert!(down(run(store.load(s.id())).await.map(|_| ())));
  assert!(down(run(store.remove(s.id())).await.map(|_| ())));
  assert!(down(run(store.remove_user("x")).await.map(|_| ())));
  assert!(down(run(store.purge_expired(at(0))).await.map(|_| ())));
  // The eager layer fails its build with a storage error.
  let program: Effect<RedisClient, SessionError, RedisEnv> =
    need::<RedisClient>().map(|c| (*c).clone());
  let built = run(program.provide_layer(RedisClient::layer(config))).await;
  assert!(matches!(built, Err(SessionError::Storage(m)) if m.contains("redis connection")));
  let _: SessionError = RedisError::Decode("x".into()).into();
}
