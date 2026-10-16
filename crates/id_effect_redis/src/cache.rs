//! [`RedisCacheBackend`]: Redis as the [`CacheBackend`] of an `id_effect` [`Cache`](id_effect::Cache).

use std::fmt;
use std::marker::PhantomData;
use std::time::Duration;

use id_effect::{CacheBackend, CacheBackendError, Effect};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::client::RedisClient;
use crate::store::RedisKv;

/// The values of a [`Cache`](id_effect::Cache) in Redis, shared by every process that uses the
/// same namespace: `Cache::with_backend(RedisCacheBackend::new(..), lookup)` has the API, and
/// the single-flight lookups, of the in-process cache.
///
/// Values are JSON strings under `namespace:key` (the key's `Display`), each expiring `ttl`
/// after it was stored (`None`: kept until invalidated or evicted by the server's
/// `maxmemory-policy`). [`invalidate_all`](CacheBackend::invalidate_all) and
/// [`size`](CacheBackend::size) scan the namespace.
///
/// ```no_run
/// use std::time::Duration;
/// use id_effect::{Cache, Effect, sync};
/// use id_effect_redis::{RedisCacheBackend, RedisClient};
///
/// fn prices(redis: &RedisClient) -> Cache<String, u64> {
///   let backend = RedisCacheBackend::new(redis.clone(), "prices", Some(Duration::from_secs(30)));
///   Cache::with_backend(backend, |sku: String| sync(move || sku.len() as u64 * 100))
/// }
/// ```
pub struct RedisCacheBackend<K, V> {
  kv: RedisKv<V>,
  ttl: Option<Duration>,
  _key: PhantomData<fn(K)>,
}

impl<K, V> Clone for RedisCacheBackend<K, V> {
  fn clone(&self) -> Self {
    Self {
      kv: self.kv.clone(),
      ttl: self.ttl,
      _key: PhantomData,
    }
  }
}

impl<K, V> fmt::Debug for RedisCacheBackend<K, V> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RedisCacheBackend")
      .field("kv", &self.kv)
      .field("ttl", &self.ttl)
      .finish()
  }
}

impl<K, V> RedisCacheBackend<K, V>
where
  V: Serialize + DeserializeOwned + Send + 'static,
{
  /// Values under `namespace`, each kept for `ttl` (`None`: no expiry).
  pub fn new(client: RedisClient, namespace: impl Into<String>, ttl: Option<Duration>) -> Self {
    Self {
      kv: RedisKv::new(client, namespace),
      ttl,
      _key: PhantomData,
    }
  }

  /// The typed store holding the values (to inspect or seed them).
  pub fn store(&self) -> &RedisKv<V> {
    &self.kv
  }
}

impl<K, V> CacheBackend<K, V> for RedisCacheBackend<K, V>
where
  K: fmt::Display + 'static,
  V: Serialize + DeserializeOwned + Send + 'static,
{
  fn get(&self, key: &K) -> Effect<Option<V>, CacheBackendError> {
    self
      .kv
      .get(&key.to_string())
      .map_err(CacheBackendError::from)
  }

  fn set(&self, key: K, value: V) -> Effect<(), CacheBackendError> {
    let key = key.to_string();
    match self.ttl {
      Some(ttl) => self.kv.set_ex(&key, &value, ttl),
      None => self.kv.set(&key, &value),
    }
    .map_err(CacheBackendError::from)
  }

  fn invalidate(&self, key: &K) -> Effect<(), CacheBackendError> {
    self
      .kv
      .delete(&key.to_string())
      .map(|_| ())
      .map_err(CacheBackendError::from)
  }

  fn invalidate_all(&self) -> Effect<(), CacheBackendError> {
    self.kv.clear().map(|_| ()).map_err(CacheBackendError::from)
  }

  fn size(&self) -> Effect<usize, CacheBackendError> {
    self
      .kv
      .keys()
      .map(|keys| keys.len())
      .map_err(CacheBackendError::from)
  }
}
