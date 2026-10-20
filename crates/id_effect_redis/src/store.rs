//! Typed stores over [`RedisClient`]: [`RedisKv`] (key → value), [`RedisHash`] (key → field →
//! value) and [`RedisJson`] (documents in RedisJSON). Values are `serde` types, stored as JSON.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::time::Duration;

use id_effect::observability::posture::record_dangerous;
use id_effect::{Effect, succeed};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::RedisError;
use crate::client::{RedisClient, namespace_glob, namespaced};

/// The JSON text of `value`.
fn encode<T: Serialize>(value: &T) -> Result<String, RedisError> {
  Ok(serde_json::to_string(value)?)
}

/// `T` from its JSON text.
fn decode<T: DeserializeOwned>(text: &str) -> Result<T, RedisError> {
  Ok(serde_json::from_str(text)?)
}

/// `T` from an optional JSON text.
fn decode_opt<T: DeserializeOwned>(text: Option<String>) -> Result<Option<T>, RedisError> {
  text.as_deref().map(decode).transpose()
}

/// A failed encoding as an effect, or `f(text)`.
fn with_encoded<T: Serialize, A: Send + 'static>(
  value: &T,
  f: impl FnOnce(String) -> Effect<A, RedisError>,
) -> Effect<A, RedisError> {
  match encode(value) {
    Ok(text) => f(text),
    Err(e) => id_effect::fail(e),
  }
}

fn millis(ttl: Duration) -> u64 {
  u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX).max(1)
}

/// Shared shape of the three stores: a client, a namespace, the value type.
struct Space<V> {
  client: RedisClient,
  namespace: String,
  _value: PhantomData<fn() -> V>,
}

impl<V> Clone for Space<V> {
  fn clone(&self) -> Self {
    Self {
      client: self.client.clone(),
      namespace: self.namespace.clone(),
      _value: PhantomData,
    }
  }
}

impl<V> Space<V> {
  fn new(client: RedisClient, namespace: impl Into<String>) -> Self {
    Self {
      client,
      namespace: namespace.into(),
      _value: PhantomData,
    }
  }

  fn key(&self, key: &str) -> String {
    namespaced(&self.namespace, key)
  }

  /// The keys of the namespace, without the `namespace:` prefix.
  fn keys(&self) -> Effect<Vec<String>, RedisError> {
    let prefix = namespaced(&self.namespace, "");
    self
      .client
      .scan(namespace_glob(&self.namespace))
      .map(move |keys| {
        keys
          .into_iter()
          .map(|k| k.strip_prefix(&prefix).unwrap_or(&k).to_owned())
          .collect()
      })
  }

  /// [`RedisError::EmptyNamespace`] when the namespace is empty: its glob is every key (M17).
  fn clear(&self) -> Effect<u64, RedisError> {
    if self.namespace.is_empty() {
      return id_effect::fail(RedisError::EmptyNamespace);
    }
    self.client.delete_matching(namespace_glob(&self.namespace))
  }

  /// [`clear`](Self::clear), the empty namespace included (every key), recorded as the posture
  /// opt-out `id`.
  fn clear_all_dangerous(&self, id: &'static str) -> Effect<u64, RedisError> {
    record_dangerous(
      id,
      "M17: clearing a store with an empty namespace deletes every key of the database",
    );
    self.client.delete_matching(namespace_glob(&self.namespace))
  }

  fn delete(&self, key: &str) -> Effect<bool, RedisError> {
    self
      .client
      .query::<u64>(redis::cmd("UNLINK").arg(self.key(key)).clone())
      .map(|n| n > 0)
  }

  fn exists(&self, key: &str) -> Effect<bool, RedisError> {
    self
      .client
      .query::<u64>(redis::cmd("EXISTS").arg(self.key(key)).clone())
      .map(|n| n > 0)
  }

  fn expire(&self, key: &str, ttl: Duration) -> Effect<bool, RedisError> {
    self
      .client
      .query::<u64>(
        redis::cmd("PEXPIRE")
          .arg(self.key(key))
          .arg(millis(ttl))
          .clone(),
      )
      .map(|n| n > 0)
  }

  fn ttl(&self, key: &str) -> Effect<Option<Duration>, RedisError> {
    self
      .client
      .query::<i64>(redis::cmd("PTTL").arg(self.key(key)).clone())
      .map(|ms| u64::try_from(ms).ok().map(Duration::from_millis))
  }
}

impl<V> fmt::Debug for Space<V> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.namespace)
  }
}

/// Typed key-value store: values of `V` (any `serde` type, stored as JSON strings) under
/// `namespace:key`, optionally with a time-to-live. Clones share the client.
///
/// ```no_run
/// use std::time::Duration;
/// use id_effect::Effect;
/// use id_effect_redis::{RedisClient, RedisError};
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// struct Profile {
///   name: String,
/// }
///
/// fn remember(redis: &RedisClient, id: &str, p: Profile) -> Effect<Option<Profile>, RedisError> {
///   let profiles = redis.kv::<Profile>("profiles");
///   let again = profiles.clone();
///   let id2 = id.to_owned();
///   profiles
///     .set_ex(id, &p, Duration::from_secs(3600))
///     .flat_map(move |()| again.get(&id2))
/// }
/// ```
pub struct RedisKv<V> {
  space: Space<V>,
}

impl<V> Clone for RedisKv<V> {
  fn clone(&self) -> Self {
    Self {
      space: self.space.clone(),
    }
  }
}

impl<V> fmt::Debug for RedisKv<V> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("RedisKv").field(&self.space).finish()
  }
}

impl<V: Serialize + DeserializeOwned + Send + 'static> RedisKv<V> {
  /// The store of `namespace` (no prefix when empty).
  pub fn new(client: RedisClient, namespace: impl Into<String>) -> Self {
    Self {
      space: Space::new(client, namespace),
    }
  }

  /// The Redis key of `key`.
  pub fn key(&self, key: &str) -> String {
    self.space.key(key)
  }

  /// The value of `key`, if any. A stored value that is not a `V` is a [`RedisError::Decode`].
  pub fn get(&self, key: &str) -> Effect<Option<V>, RedisError> {
    self
      .space
      .client
      .query::<Option<String>>(redis::cmd("GET").arg(self.key(key)).clone())
      .flat_map(|text| effect(decode_opt(text)))
  }

  /// The values of `keys`, in order (`MGET`, one round trip).
  pub fn get_many(&self, keys: &[&str]) -> Effect<Vec<Option<V>>, RedisError> {
    if keys.is_empty() {
      return succeed(Vec::new());
    }
    let full: Vec<String> = keys.iter().map(|k| self.key(k)).collect();
    self
      .space
      .client
      .query::<Vec<Option<String>>>(redis::cmd("MGET").arg(full).clone())
      .flat_map(|texts| effect(texts.into_iter().map(decode_opt).collect()))
  }

  /// Store `value` under `key` (no expiry; an existing time-to-live is cleared).
  pub fn set(&self, key: &str, value: &V) -> Effect<(), RedisError> {
    let key = self.key(key);
    let client = self.space.client.clone();
    with_encoded(value, move |text| {
      client.query::<()>(redis::cmd("SET").arg(key).arg(text).clone())
    })
  }

  /// Store `value` under `key` for `ttl` (rounded to milliseconds, at least 1).
  pub fn set_ex(&self, key: &str, value: &V, ttl: Duration) -> Effect<(), RedisError> {
    let key = self.key(key);
    let client = self.space.client.clone();
    with_encoded(value, move |text| {
      client.query::<()>(
        redis::cmd("SET")
          .arg(key)
          .arg(text)
          .arg("PX")
          .arg(millis(ttl))
          .clone(),
      )
    })
  }

  /// Store `value` under `key` only if `key` is absent (with `ttl`, if given); `true` when it
  /// was stored. A building block for locks and idempotency keys.
  pub fn set_nx(&self, key: &str, value: &V, ttl: Option<Duration>) -> Effect<bool, RedisError> {
    let key = self.key(key);
    let client = self.space.client.clone();
    with_encoded(value, move |text| {
      let mut cmd = redis::cmd("SET");
      cmd.arg(key).arg(text).arg("NX");
      if let Some(ttl) = ttl {
        cmd.arg("PX").arg(millis(ttl));
      }
      client
        .query::<Option<String>>(cmd)
        .map(|reply| reply.is_some())
    })
  }

  /// Add `delta` to the integer stored under `key` (0 when absent) and return the result. The
  /// value must be an integer (`V` = `i64`, `u32`, …).
  pub fn incr_by(&self, key: &str, delta: i64) -> Effect<i64, RedisError> {
    self
      .space
      .client
      .query::<i64>(redis::cmd("INCRBY").arg(self.key(key)).arg(delta).clone())
  }

  /// Remove `key`; `true` if it existed.
  pub fn delete(&self, key: &str) -> Effect<bool, RedisError> {
    self.space.delete(key)
  }

  /// `true` if `key` exists.
  pub fn exists(&self, key: &str) -> Effect<bool, RedisError> {
    self.space.exists(key)
  }

  /// Expire `key` after `ttl`; `false` if it does not exist.
  pub fn expire(&self, key: &str, ttl: Duration) -> Effect<bool, RedisError> {
    self.space.expire(key, ttl)
  }

  /// The remaining time-to-live of `key`: `None` when it has none or does not exist.
  pub fn ttl(&self, key: &str) -> Effect<Option<Duration>, RedisError> {
    self.space.ttl(key)
  }

  /// Every key of the namespace (without the prefix), sorted. Scans, so it is linear in the
  /// size of the database.
  pub fn keys(&self) -> Effect<Vec<String>, RedisError> {
    self.space.keys()
  }

  /// Remove every key of the namespace; how many were removed. [`RedisError::EmptyNamespace`]
  /// when the namespace is empty ([`clear_all_dangerous`](Self::clear_all_dangerous) flushes).
  pub fn clear(&self) -> Effect<u64, RedisError> {
    self.space.clear()
  }

  /// Remove every key of the namespace, or of the whole database when the namespace is empty;
  /// how many were removed. Records the posture opt-out
  /// `id_effect_redis::RedisKv::clear_all_dangerous`.
  pub fn clear_all_dangerous(&self) -> Effect<u64, RedisError> {
    self
      .space
      .clear_all_dangerous("id_effect_redis::RedisKv::clear_all_dangerous")
  }
}

/// Typed hash store: under each `namespace:key` a Redis hash of fields to `V` (stored as JSON
/// strings). Clones share the client.
pub struct RedisHash<V> {
  space: Space<V>,
}

impl<V> Clone for RedisHash<V> {
  fn clone(&self) -> Self {
    Self {
      space: self.space.clone(),
    }
  }
}

impl<V> fmt::Debug for RedisHash<V> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("RedisHash").field(&self.space).finish()
  }
}

impl<V: Serialize + DeserializeOwned + Send + 'static> RedisHash<V> {
  /// The store of `namespace` (no prefix when empty).
  pub fn new(client: RedisClient, namespace: impl Into<String>) -> Self {
    Self {
      space: Space::new(client, namespace),
    }
  }

  /// The Redis key of `key`.
  pub fn key(&self, key: &str) -> String {
    self.space.key(key)
  }

  /// Field `field` of `key`, if any.
  pub fn get(&self, key: &str, field: &str) -> Effect<Option<V>, RedisError> {
    self
      .space
      .client
      .query::<Option<String>>(redis::cmd("HGET").arg(self.key(key)).arg(field).clone())
      .flat_map(|text| effect(decode_opt(text)))
  }

  /// Every field of `key` (empty when `key` does not exist).
  pub fn get_all(&self, key: &str) -> Effect<BTreeMap<String, V>, RedisError> {
    self
      .space
      .client
      .query::<BTreeMap<String, String>>(redis::cmd("HGETALL").arg(self.key(key)).clone())
      .flat_map(|fields| {
        effect(
          fields
            .into_iter()
            .map(|(f, text)| decode(&text).map(|v| (f, v)))
            .collect(),
        )
      })
  }

  /// Set field `field` of `key` to `value`; `true` if the field is new.
  pub fn set(&self, key: &str, field: &str, value: &V) -> Effect<bool, RedisError> {
    let (key, field) = (self.key(key), field.to_owned());
    let client = self.space.client.clone();
    with_encoded(value, move |text| {
      client
        .query::<u64>(redis::cmd("HSET").arg(key).arg(field).arg(text).clone())
        .map(|added| added > 0)
    })
  }

  /// Set several fields of `key` at once (`HSET` with every pair, atomically).
  pub fn set_all(&self, key: &str, fields: &BTreeMap<String, V>) -> Effect<(), RedisError> {
    if fields.is_empty() {
      return succeed(());
    }
    let mut cmd = redis::cmd("HSET");
    cmd.arg(self.key(key));
    for (field, value) in fields {
      match encode(value) {
        Ok(text) => {
          cmd.arg(field).arg(text);
        }
        Err(e) => return id_effect::fail(e),
      }
    }
    self.space.client.query::<u64>(cmd).map(|_| ())
  }

  /// Remove field `field` of `key`; `true` if it existed.
  pub fn delete_field(&self, key: &str, field: &str) -> Effect<bool, RedisError> {
    self
      .space
      .client
      .query::<u64>(redis::cmd("HDEL").arg(self.key(key)).arg(field).clone())
      .map(|n| n > 0)
  }

  /// Add `delta` to the integer field `field` of `key` (0 when absent) and return the result.
  pub fn incr_by(&self, key: &str, field: &str, delta: i64) -> Effect<i64, RedisError> {
    self.space.client.query::<i64>(
      redis::cmd("HINCRBY")
        .arg(self.key(key))
        .arg(field)
        .arg(delta)
        .clone(),
    )
  }

  /// The number of fields of `key`.
  pub fn len(&self, key: &str) -> Effect<u64, RedisError> {
    self
      .space
      .client
      .query::<u64>(redis::cmd("HLEN").arg(self.key(key)).clone())
  }

  /// Remove the whole hash `key`; `true` if it existed.
  pub fn delete(&self, key: &str) -> Effect<bool, RedisError> {
    self.space.delete(key)
  }

  /// Expire the whole hash `key` after `ttl`; `false` if it does not exist.
  pub fn expire(&self, key: &str, ttl: Duration) -> Effect<bool, RedisError> {
    self.space.expire(key, ttl)
  }

  /// Every key of the namespace (without the prefix), sorted.
  pub fn keys(&self) -> Effect<Vec<String>, RedisError> {
    self.space.keys()
  }

  /// Remove every hash of the namespace; how many were removed. [`RedisError::EmptyNamespace`]
  /// when the namespace is empty ([`clear_all_dangerous`](Self::clear_all_dangerous) flushes).
  pub fn clear(&self) -> Effect<u64, RedisError> {
    self.space.clear()
  }

  /// Remove every hash of the namespace, or every key of the database when the namespace is
  /// empty; how many were removed. Records the posture opt-out
  /// `id_effect_redis::RedisHash::clear_all_dangerous`.
  pub fn clear_all_dangerous(&self) -> Effect<u64, RedisError> {
    self
      .space
      .clear_all_dangerous("id_effect_redis::RedisHash::clear_all_dangerous")
  }
}

/// Document store over the RedisJSON module (built into Redis 8 and Redis Stack): documents of
/// `D` under `namespace:id`, read and updated whole or at a JSONPath. Clones share the client.
///
/// Without the module every command fails with [`RedisError::Command`] (unknown command).
///
/// ```no_run
/// use id_effect::Effect;
/// use id_effect_redis::{RedisClient, RedisError};
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// struct Order {
///   status: String,
///   total_cents: i64,
/// }
///
/// fn ship(redis: &RedisClient, id: &str) -> Effect<bool, RedisError> {
///   redis.json::<Order>("orders").set_path(id, "$.status", &"shipped")
/// }
/// ```
pub struct RedisJson<D> {
  space: Space<D>,
}

impl<D> Clone for RedisJson<D> {
  fn clone(&self) -> Self {
    Self {
      space: self.space.clone(),
    }
  }
}

impl<D> fmt::Debug for RedisJson<D> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("RedisJson").field(&self.space).finish()
  }
}

impl<D: Serialize + DeserializeOwned + Send + 'static> RedisJson<D> {
  /// The store of `namespace` (no prefix when empty).
  pub fn new(client: RedisClient, namespace: impl Into<String>) -> Self {
    Self {
      space: Space::new(client, namespace),
    }
  }

  /// The Redis key of document `id`.
  pub fn key(&self, id: &str) -> String {
    self.space.key(id)
  }

  /// Store `doc` as document `id`, replacing it.
  pub fn set(&self, id: &str, doc: &D) -> Effect<(), RedisError> {
    let key = self.key(id);
    let client = self.space.client.clone();
    with_encoded(doc, move |text| {
      client.query::<()>(redis::cmd("JSON.SET").arg(key).arg("$").arg(text).clone())
    })
  }

  /// Document `id`, if it exists.
  pub fn get(&self, id: &str) -> Effect<Option<D>, RedisError> {
    self
      .space
      .client
      .query::<Option<String>>(redis::cmd("JSON.GET").arg(self.key(id)).arg("$").clone())
      .flat_map(|text| effect(first_match(text)))
  }

  /// The value at JSONPath `path` (e.g. `$.status`) of document `id`: the first match, `None`
  /// when the document or the path does not exist.
  pub fn get_path<T: DeserializeOwned + Send + 'static>(
    &self,
    id: &str,
    path: &str,
  ) -> Effect<Option<T>, RedisError> {
    self
      .space
      .client
      .query::<Option<String>>(redis::cmd("JSON.GET").arg(self.key(id)).arg(path).clone())
      .flat_map(|text| effect(first_match(text)))
  }

  /// Set the value at JSONPath `path` of an existing document `id`; `false` when the document
  /// or the parent of `path` does not exist.
  pub fn set_path<T: Serialize>(
    &self,
    id: &str,
    path: &str,
    value: &T,
  ) -> Effect<bool, RedisError> {
    let (key, path) = (self.key(id), path.to_owned());
    let client = self.space.client.clone();
    with_encoded(value, move |text| {
      client
        .query::<Option<String>>(
          redis::cmd("JSON.SET")
            .arg(key)
            .arg(path)
            .arg(text)
            .arg("XX")
            .clone(),
        )
        .map(|reply| reply.is_some())
    })
  }

  /// Add `delta` to the number at JSONPath `path` of document `id`; the new value of the first
  /// match.
  pub fn incr_by(&self, id: &str, path: &str, delta: f64) -> Effect<Option<f64>, RedisError> {
    self
      .space
      .client
      .query::<Option<String>>(
        redis::cmd("JSON.NUMINCRBY")
          .arg(self.key(id))
          .arg(path)
          .arg(delta)
          .clone(),
      )
      .flat_map(|text| effect(first_match(text)))
  }

  /// Remove the values at JSONPath `path` of document `id`; how many were removed.
  pub fn delete_path(&self, id: &str, path: &str) -> Effect<u64, RedisError> {
    self
      .space
      .client
      .query::<u64>(redis::cmd("JSON.DEL").arg(self.key(id)).arg(path).clone())
  }

  /// Remove document `id`; `true` if it existed.
  pub fn delete(&self, id: &str) -> Effect<bool, RedisError> {
    self.space.delete(id)
  }

  /// Every document id of the namespace, sorted.
  pub fn ids(&self) -> Effect<Vec<String>, RedisError> {
    self.space.keys()
  }

  /// Remove every document of the namespace; how many were removed.
  /// [`RedisError::EmptyNamespace`] when the namespace is empty
  /// ([`clear_all_dangerous`](Self::clear_all_dangerous) flushes).
  pub fn clear(&self) -> Effect<u64, RedisError> {
    self.space.clear()
  }

  /// Remove every document of the namespace, or every key of the database when the namespace is
  /// empty; how many were removed. Records the posture opt-out
  /// `id_effect_redis::RedisJson::clear_all_dangerous`.
  pub fn clear_all_dangerous(&self) -> Effect<u64, RedisError> {
    self
      .space
      .clear_all_dangerous("id_effect_redis::RedisJson::clear_all_dangerous")
  }
}

/// The first element of a JSONPath reply (a JSON array of matches).
fn first_match<T: DeserializeOwned>(text: Option<String>) -> Result<Option<T>, RedisError> {
  let Some(text) = text else {
    return Ok(None);
  };
  let matches: Vec<serde_json::Value> = decode(&text)?;
  matches
    .into_iter()
    .next()
    .map(|v| serde_json::from_value(v).map_err(RedisError::from))
    .transpose()
}

/// A ready result as an effect.
fn effect<A: Send + 'static>(r: Result<A, RedisError>) -> Effect<A, RedisError> {
  match r {
    Ok(a) => succeed(a),
    Err(e) => id_effect::fail(e),
  }
}

impl RedisClient {
  /// The typed key-value store of `namespace`.
  pub fn kv<V: Serialize + DeserializeOwned + Send + 'static>(
    &self,
    namespace: impl Into<String>,
  ) -> RedisKv<V> {
    RedisKv::new(self.clone(), namespace)
  }

  /// The typed hash store of `namespace`.
  pub fn hash<V: Serialize + DeserializeOwned + Send + 'static>(
    &self,
    namespace: impl Into<String>,
  ) -> RedisHash<V> {
    RedisHash::new(self.clone(), namespace)
  }

  /// The JSON document store of `namespace` (needs the RedisJSON module).
  pub fn json<D: Serialize + DeserializeOwned + Send + 'static>(
    &self,
    namespace: impl Into<String>,
  ) -> RedisJson<D> {
    RedisJson::new(self.clone(), namespace)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn json_helpers() {
    assert_eq!(first_match::<u32>(None), Ok(None));
    assert_eq!(first_match::<u32>(Some("[]".into())), Ok(None));
    assert_eq!(first_match::<u32>(Some("[7,8]".into())), Ok(Some(7)));
    assert!(matches!(
      first_match::<u32>(Some("[\"x\"]".into())),
      Err(RedisError::Decode(_))
    ));
    assert!(matches!(
      first_match::<u32>(Some("{".into())),
      Err(RedisError::Decode(_))
    ));
    assert_eq!(decode_opt::<u32>(Some("5".into())), Ok(Some(5)));
    assert_eq!(decode_opt::<u32>(None), Ok(None));
    assert_eq!(millis(Duration::from_micros(10)), 1);
    assert_eq!(millis(Duration::from_secs(2)), 2000);
  }

  /// A value whose serialization fails.
  struct Unencodable;

  impl Serialize for Unencodable {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
      Err(serde::ser::Error::custom("no"))
    }
  }

  #[test]
  fn an_unencodable_value_fails_before_any_io() {
    let eff: Effect<(), RedisError> = with_encoded(&Unencodable, |_| succeed(()));
    let exit = eff.block_on();
    assert!(
      matches!(exit, id_effect::Exit::Failure(id_effect::Cause::Fail(RedisError::Decode(ref m))) if m == "no"),
      "{exit:?}"
    );
  }
}
