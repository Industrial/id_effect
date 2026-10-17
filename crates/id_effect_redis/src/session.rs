//! [`RedisSessionStore`]: id_effect_auth's [`SessionStore`] in Redis (feature `session`).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::LazyLock;

use id_effect::{Effect, Layer, UtcDateTime, hlist, need};
use id_effect_auth::session::{
  Session, SessionError, SessionId, SessionStore, SessionStoreEnv, SessionStoreService,
};
use redis::Script;
use sha2::{Digest, Sha256};

use crate::RedisError;
use crate::client::{RedisClient, RedisEnv};

/// Upsert. KEYS: record, owners, expiry index, user set. ARGV: user-set prefix, id hash, user,
/// attrs, created, last seen, idle expiry, absolute expiry, expiry (µs), expiry (ms, ceil).
static SAVE: LazyLock<Script> = LazyLock::new(|| {
  Script::new(
    r"
local old = redis.call('HGET', KEYS[2], ARGV[2])
if old and old ~= ARGV[3] then redis.call('SREM', ARGV[1] .. old, ARGV[2]) end
redis.call('DEL', KEYS[1])
redis.call('HSET', KEYS[1], 'user_id', ARGV[3], 'attrs', ARGV[4], 'created_at', ARGV[5],
  'last_seen_at', ARGV[6], 'idle_expires_at', ARGV[7], 'absolute_expires_at', ARGV[8])
redis.call('PEXPIREAT', KEYS[1], ARGV[10])
redis.call('HSET', KEYS[2], ARGV[2], ARGV[3])
redis.call('SADD', KEYS[4], ARGV[2])
redis.call('ZADD', KEYS[3], ARGV[9], ARGV[2])
return 1
",
  )
});

/// Remove one. KEYS: record, owners, expiry index. ARGV: user-set prefix, id hash.
static REMOVE: LazyLock<Script> = LazyLock::new(|| {
  Script::new(
    r"
local n = redis.call('DEL', KEYS[1])
local user = redis.call('HGET', KEYS[2], ARGV[2])
if user then redis.call('SREM', ARGV[1] .. user, ARGV[2]) end
redis.call('HDEL', KEYS[2], ARGV[2])
redis.call('ZREM', KEYS[3], ARGV[2])
return n
",
  )
});

/// Remove a user's. KEYS: user set, owners, expiry index. ARGV: record prefix.
static REMOVE_USER: LazyLock<Script> = LazyLock::new(|| {
  Script::new(
    r"
local n = 0
for _, h in ipairs(redis.call('SMEMBERS', KEYS[1])) do
  n = n + redis.call('DEL', ARGV[1] .. h)
  redis.call('HDEL', KEYS[2], h)
  redis.call('ZREM', KEYS[3], h)
end
redis.call('DEL', KEYS[1])
return n
",
  )
});

/// Purge expired. KEYS: expiry index, owners. ARGV: now (µs), record prefix, user-set prefix.
static PURGE: LazyLock<Script> = LazyLock::new(|| {
  Script::new(
    r"
local ids = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
for _, h in ipairs(ids) do
  redis.call('DEL', ARGV[2] .. h)
  local user = redis.call('HGET', KEYS[2], h)
  if user then redis.call('SREM', ARGV[3] .. user, h) end
  redis.call('HDEL', KEYS[2], h)
end
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
return #ids
",
  )
});

/// [`SessionStore`] in Redis, over a [`RedisClient`]. Clones share the client.
///
/// - A record is a hash under `{prefix}:session:{sha256(id)}`; the id itself (the cookie value)
///   is never written, as in `PgSessionStore`. Attributes are a JSON object; the four times are
///   RFC 3339 timestamps, stored losslessly.
/// - A record expires in Redis at the session's [`expires_at`](Session::expires_at), so an
///   abandoned session needs no cleanup job; [`purge_expired`](SessionStore::purge_expired)
///   still removes (and counts) every record whose expiry is at or before `now`, from the
///   expiry index `{prefix}:session-expiry`.
/// - `{prefix}:session-user:{user_id}` sets index sessions by user for
///   [`remove_user`](SessionStore::remove_user).
/// - Each operation is one Lua script, so a record and its indexes change atomically. The keys
///   are not hash-tagged: use a single Redis node (or a primary with replicas), not a cluster.
///
/// Failures surface as [`SessionError::Storage`].
#[derive(Clone)]
pub struct RedisSessionStore {
  client: RedisClient,
  prefix: String,
}

impl fmt::Debug for RedisSessionStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RedisSessionStore")
      .field("prefix", &self.prefix)
      .finish_non_exhaustive()
  }
}

impl RedisSessionStore {
  /// Default key prefix.
  pub const DEFAULT_PREFIX: &'static str = "id_effect";

  /// A store over `client`, under [`DEFAULT_PREFIX`](Self::DEFAULT_PREFIX).
  pub fn new(client: RedisClient) -> Self {
    Self {
      client,
      prefix: Self::DEFAULT_PREFIX.to_owned(),
    }
  }

  /// Keep the keys under `prefix` instead (several applications on one server).
  #[must_use]
  pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
    self.prefix = prefix.into();
    self
  }

  fn record_prefix(&self) -> String {
    format!("{}:session:", self.prefix)
  }

  fn user_prefix(&self) -> String {
    format!("{}:session-user:", self.prefix)
  }

  fn owners(&self) -> String {
    format!("{}:session-owner", self.prefix)
  }

  fn expiry(&self) -> String {
    format!("{}:session-expiry", self.prefix)
  }
}

/// The record key suffix of `id`: lowercase hex SHA-256 of the id.
fn id_hash(id: &SessionId) -> String {
  Sha256::digest(id.as_str().as_bytes())
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect()
}

fn storage(e: RedisError) -> SessionError {
  SessionError::Storage(e.to_string())
}

/// A Redis failure as [`SessionError::Storage`] (so a session program can build
/// [`RedisClient::layer`] with its own error type).
impl From<RedisError> for SessionError {
  fn from(e: RedisError) -> Self {
    storage(e)
  }
}

fn micros(t: UtcDateTime) -> i64 {
  t.0.as_microsecond()
}

/// Milliseconds since the epoch, rounded up (a record never expires before its session).
fn millis_ceil(t: UtcDateTime) -> i64 {
  micros(t).div_euclid(1000) + i64::from(micros(t).rem_euclid(1000) > 0)
}

/// The session `id` from its record fields.
fn decode(id: SessionId, mut fields: HashMap<String, String>) -> Result<Session, String> {
  let mut take = |name: &str| {
    fields
      .remove(name)
      .ok_or_else(|| format!("missing field {name}"))
  };
  let at = |text: String| {
    text
      .parse::<jiff::Timestamp>()
      .map(UtcDateTime)
      .map_err(|e| format!("bad timestamp {text:?}: {e}"))
  };
  let user = take("user_id")?;
  let attrs: BTreeMap<String, String> =
    serde_json::from_str(&take("attrs")?).map_err(|e| format!("bad attrs: {e}"))?;
  Ok(Session::from_parts(
    id,
    user,
    attrs,
    at(take("created_at")?)?,
    at(take("last_seen_at")?)?,
    at(take("idle_expires_at")?)?,
    at(take("absolute_expires_at")?)?,
  ))
}

impl SessionStore for RedisSessionStore {
  fn save(&self, session: Session) -> Effect<(), SessionError> {
    let hash = id_hash(session.id());
    let attrs = match serde_json::to_string(session.attrs()) {
      Ok(attrs) => attrs,
      Err(e) => return id_effect::fail(SessionError::Storage(e.to_string())),
    };
    let user = session.user_id().to_owned();
    let expires = session.expires_at();
    let keys = vec![
      format!("{}{hash}", self.record_prefix()),
      self.owners(),
      self.expiry(),
      format!("{}{user}", self.user_prefix()),
    ];
    let args = vec![
      self.user_prefix(),
      hash,
      user,
      attrs,
      session.created_at().0.to_string(),
      session.last_seen_at().0.to_string(),
      session.idle_expires_at().0.to_string(),
      session.absolute_expires_at().0.to_string(),
      micros(expires).to_string(),
      millis_ceil(expires).to_string(),
    ];
    self
      .client
      .invoke::<i64>(&SAVE, keys, args)
      .map(|_| ())
      .map_err(storage)
  }

  fn load(&self, id: &SessionId) -> Effect<Option<Session>, SessionError> {
    let id = id.clone();
    self
      .client
      .query::<HashMap<String, String>>(
        redis::cmd("HGETALL")
          .arg(format!("{}{}", self.record_prefix(), id_hash(&id)))
          .clone(),
      )
      .map_err(storage)
      .flat_map(move |fields| {
        if fields.is_empty() {
          return id_effect::succeed(None);
        }
        match decode(id, fields) {
          Ok(session) => id_effect::succeed(Some(session)),
          Err(m) => id_effect::fail(SessionError::Storage(m)),
        }
      })
  }

  fn remove(&self, id: &SessionId) -> Effect<bool, SessionError> {
    let hash = id_hash(id);
    let keys = vec![
      format!("{}{hash}", self.record_prefix()),
      self.owners(),
      self.expiry(),
    ];
    self
      .client
      .invoke::<i64>(&REMOVE, keys, vec![self.user_prefix(), hash])
      .map(|n| n > 0)
      .map_err(storage)
  }

  fn remove_user(&self, user_id: &str) -> Effect<u64, SessionError> {
    let keys = vec![
      format!("{}{user_id}", self.user_prefix()),
      self.owners(),
      self.expiry(),
    ];
    self
      .client
      .invoke::<u64>(&REMOVE_USER, keys, vec![self.record_prefix()])
      .map_err(storage)
  }

  fn purge_expired(&self, now: UtcDateTime) -> Effect<u64, SessionError> {
    let keys = vec![self.expiry(), self.owners()];
    let args = vec![
      micros(now).to_string(),
      self.record_prefix(),
      self.user_prefix(),
    ];
    self
      .client
      .invoke::<u64>(&PURGE, keys, args)
      .map_err(storage)
  }
}

/// A [`Layer`] providing a [`RedisSessionStore`] (default prefix) as the
/// [`SessionStoreService`], over the [`RedisClient`] it needs (feed it [`RedisClient::layer`]
/// with `provide_to`).
pub fn redis_session_store_layer<E: Send + 'static>() -> Layer<SessionStoreEnv, E, RedisEnv> {
  Layer::effect(|| {
    need::<RedisClient>().map(|client| {
      hlist![SessionStoreService::new(RedisSessionStore::new(
        (*client).clone()
      ))]
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn at_micros(us: i64) -> UtcDateTime {
    UtcDateTime(jiff::Timestamp::from_microsecond(us).unwrap())
  }

  #[test]
  fn expiry_rounds_up_to_the_millisecond() {
    assert_eq!(millis_ceil(at_micros(5_000)), 5);
    assert_eq!(millis_ceil(at_micros(5_001)), 6);
    assert_eq!(millis_ceil(at_micros(-1)), 0);
    assert_eq!(micros(at_micros(42)), 42);
  }

  #[test]
  fn malformed_records_are_described() {
    let id = SessionId::generate().unwrap();
    let mut fields: HashMap<String, String> = [
      ("user_id", "u"),
      ("attrs", "{}"),
      ("created_at", "2027-01-01T00:00:00Z"),
      ("last_seen_at", "2027-01-01T00:00:00.000000001Z"),
      ("idle_expires_at", "2027-01-01T00:30:00Z"),
      ("absolute_expires_at", "2027-01-01T08:00:00Z"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let s = decode(id.clone(), fields.clone()).unwrap();
    assert_eq!(s.last_seen_at().0.subsec_nanosecond(), 1);
    fields.insert("attrs".into(), "[".into());
    assert!(
      decode(id.clone(), fields.clone())
        .unwrap_err()
        .contains("bad attrs")
    );
    fields.insert("attrs".into(), "{}".into());
    fields.insert("created_at".into(), "yesterday".into());
    assert!(
      decode(id.clone(), fields.clone())
        .unwrap_err()
        .contains("bad timestamp")
    );
    fields.remove("user_id");
    assert_eq!(decode(id, fields).unwrap_err(), "missing field user_id");
  }

  #[test]
  fn keys_hash_the_id_and_debug_shows_the_prefix() {
    let id = SessionId::parse(&"0".repeat(64)).unwrap();
    assert_eq!(
      id_hash(&id),
      "60e05bd1b195af2f94112fa7197a5c88289058840ce7c6df9693756bc6250f55"
    );
    assert_eq!(
      storage(RedisError::Command("x".into())),
      SessionError::Storage("redis command: x".into())
    );
  }
}
