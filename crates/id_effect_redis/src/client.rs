//! [`RedisClient`]: pooled, reconnecting connections as an effect capability, and its layer.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use id_effect::{Effect, HCons, HNil, Layer, from_async, hlist};
#[cfg(any(feature = "session", feature = "rate-limit"))]
use redis::Script;
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{Cmd, FromRedisValue, Pipeline};

use crate::RedisError;

/// Where and how [`RedisClient`] connects.
///
/// `Debug` hides the password of the URL.
#[derive(Clone, PartialEq, Eq)]
pub struct RedisConfig {
  url: String,
  pool_size: usize,
  connection_timeout: Option<Duration>,
  response_timeout: Option<Duration>,
  reconnect_retries: Option<usize>,
}

impl fmt::Debug for RedisConfig {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RedisConfig")
      .field("url", &redacted(&self.url))
      .field("pool_size", &self.pool_size)
      .field("connection_timeout", &self.connection_timeout)
      .field("response_timeout", &self.response_timeout)
      .field("reconnect_retries", &self.reconnect_retries)
      .finish()
  }
}

/// `url` with the password of its user info replaced by `***`.
fn redacted(url: &str) -> String {
  let Some((scheme, rest)) = url.split_once("://") else {
    return url.to_owned();
  };
  match rest.rsplit_once('@') {
    Some((userinfo, host)) => {
      let user = userinfo.split_once(':').map_or("", |(user, _)| user);
      format!("{scheme}://{user}:***@{host}")
    }
    None => url.to_owned(),
  }
}

impl Default for RedisConfig {
  /// [`RedisConfig::DEFAULT_URL`].
  fn default() -> Self {
    Self::new(Self::DEFAULT_URL)
  }
}

impl RedisConfig {
  /// A local server on the default port.
  pub const DEFAULT_URL: &'static str = "redis://127.0.0.1:6379";

  /// Default [`pool_size`](Self::pool_size).
  pub const DEFAULT_POOL_SIZE: usize = 4;

  /// A server at `url` (`redis://[user:password@]host[:port][/db]`, or `rediss://` with a TLS
  /// feature of `redis`), with [`DEFAULT_POOL_SIZE`](Self::DEFAULT_POOL_SIZE) connections and
  /// redis-rs's timeouts and reconnect policy.
  pub fn new(url: impl Into<String>) -> Self {
    Self {
      url: url.into(),
      pool_size: Self::DEFAULT_POOL_SIZE,
      connection_timeout: None,
      response_timeout: None,
      reconnect_retries: None,
    }
  }

  /// The server URL.
  pub fn url(&self) -> &str {
    &self.url
  }

  /// Open `n` connections (at least 1) and spread commands over them round-robin. Each one is
  /// multiplexed (many commands in flight at once) and reconnects by itself.
  #[must_use]
  pub fn pool_size(mut self, n: usize) -> Self {
    self.pool_size = n.max(1);
    self
  }

  /// Give up a connection attempt after `timeout`.
  #[must_use]
  pub fn connection_timeout(mut self, timeout: Duration) -> Self {
    self.connection_timeout = Some(timeout);
    self
  }

  /// Fail a command whose reply has not arrived after `timeout`.
  #[must_use]
  pub fn response_timeout(mut self, timeout: Duration) -> Self {
    self.response_timeout = Some(timeout);
    self
  }

  /// Retry a lost connection `n` times (with exponential backoff) before a command fails.
  #[must_use]
  pub fn reconnect_retries(mut self, n: usize) -> Self {
    self.reconnect_retries = Some(n);
    self
  }

  fn manager_config(&self) -> ConnectionManagerConfig {
    let mut config = ConnectionManagerConfig::new();
    if let Some(t) = self.connection_timeout {
      config = config.set_connection_timeout(Some(t));
    }
    if let Some(t) = self.response_timeout {
      config = config.set_response_timeout(Some(t));
    }
    if let Some(n) = self.reconnect_retries {
      config = config.set_number_of_retries(n);
    }
    config
  }

  /// [`manager_config`](Self::manager_config) for a connection that waits on blocking commands
  /// for up to `block`: the response timeout (redis-rs's 500 ms unless set) grows by `block`.
  #[cfg(feature = "streams")]
  pub(crate) fn blocking_manager_config(&self, block: Duration) -> ConnectionManagerConfig {
    let base = self.response_timeout.unwrap_or(Duration::from_millis(500));
    self
      .manager_config()
      .set_response_timeout(Some(base + block))
  }
}

struct Inner {
  client: redis::Client,
  pool: Vec<ConnectionManager>,
  next: AtomicUsize,
  config: RedisConfig,
}

/// A Redis client: a pool of reconnecting, multiplexed connections. Clones share the pool.
///
/// It is the capability ([`RedisEnv`], built by [`RedisClient::layer`]) and the entry point to
/// the typed stores ([`kv`](Self::kv), [`hash`](Self::hash), [`json`](Self::json)), pub/sub
/// ([`publish`](Self::publish), [`subscribe`](Self::subscribe)) and raw commands
/// ([`query`](Self::query)). Every operation is a lazy [`Effect`] run on a tokio runtime (for
/// example `id_effect_tokio::TokioRuntime`).
#[derive(Clone)]
pub struct RedisClient {
  inner: Arc<Inner>,
}

impl fmt::Debug for RedisClient {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RedisClient")
      .field("url", &redacted(&self.inner.config.url))
      .field("connections", &self.inner.pool.len())
      .finish()
  }
}

/// The typed environment holding a [`RedisClient`].
pub type RedisEnv = HCons<RedisClient, HNil>;

/// The [`Layer`] that connects a [`RedisClient`] ([`RedisClient::layer`]).
pub type RedisLayer<E> = Layer<RedisEnv, E>;

impl RedisClient {
  /// Open the pool of `config` and check it with `PING`; a failure is the effect's error.
  pub fn connect(config: RedisConfig) -> Effect<Self, RedisError> {
    let client = Self::open(config, true);
    client.flat_map(|client| client.ping().map(move |()| client))
  }

  /// A client whose connections open on first use: building it does not need the server (a
  /// command fails while the server is unreachable).
  pub fn connect_lazy(config: RedisConfig) -> Effect<Self, RedisError> {
    Self::open(config, false)
  }

  fn open(config: RedisConfig, eager: bool) -> Effect<Self, RedisError> {
    from_async(move || async move {
      let client = redis::Client::open(config.url.as_str())?;
      let mut pool = Vec::with_capacity(config.pool_size);
      for _ in 0..config.pool_size {
        let manager = if eager {
          ConnectionManager::new_with_config(client.clone(), config.manager_config()).await?
        } else {
          ConnectionManager::new_lazy_with_config(client.clone(), config.manager_config())?
        };
        pool.push(manager);
      }
      Ok(Self {
        inner: Arc::new(Inner {
          client,
          pool,
          next: AtomicUsize::new(0),
          config,
        }),
      })
    })
  }

  /// The layer providing a client connected with `config` ([`connect`](Self::connect)); a
  /// failure fails the build with `E::from(err)`.
  ///
  /// ```no_run
  /// use id_effect::{Effect, need};
  /// use id_effect_redis::{RedisClient, RedisConfig, RedisEnv, RedisError};
  ///
  /// let program: Effect<(), RedisError, RedisEnv> =
  ///   need::<RedisClient>().flat_map(|redis| redis.ping().narrow());
  /// let runnable = program.provide_layer(RedisClient::layer(RedisConfig::default()));
  /// # let _ = runnable;
  /// ```
  pub fn layer<E>(config: RedisConfig) -> RedisLayer<E>
  where
    E: From<RedisError> + Send + 'static,
  {
    Layer::effect(move || {
      Self::connect(config.clone())
        .map_err(E::from)
        .map(|client| hlist![client])
    })
  }

  /// The configuration the client was built with.
  pub fn config(&self) -> &RedisConfig {
    &self.inner.config
  }

  /// The next pooled connection (round-robin), for redis-rs APIs this crate does not wrap.
  pub fn connection(&self) -> ConnectionManager {
    let i = self.inner.next.fetch_add(1, Ordering::Relaxed) % self.inner.pool.len();
    self.inner.pool[i].clone()
  }

  /// The underlying redis-rs client (dedicated connections, such as pub/sub, open from it).
  pub fn redis(&self) -> &redis::Client {
    &self.inner.client
  }

  /// Run `cmd` and decode its reply as `T`.
  ///
  /// ```no_run
  /// # use id_effect_redis::RedisClient;
  /// # fn f(redis: RedisClient) {
  /// let size = redis.query::<u64>(redis::cmd("DBSIZE"));
  /// # let _ = size;
  /// # }
  /// ```
  pub fn query<T: FromRedisValue + Send + 'static>(&self, cmd: Cmd) -> Effect<T, RedisError> {
    let mut conn = self.connection();
    from_async(move || async move { Ok(cmd.query_async(&mut conn).await?) })
  }

  /// Run `pipeline` (atomically when built with `.atomic()`) and decode its replies as `T`.
  pub fn query_pipeline<T: FromRedisValue + Send + 'static>(
    &self,
    pipeline: Pipeline,
  ) -> Effect<T, RedisError> {
    let mut conn = self.connection();
    from_async(move || async move { Ok(pipeline.query_async(&mut conn).await?) })
  }

  /// Run the Lua `script` (by hash, loading it on first use) with `keys` and `args`.
  #[cfg(any(feature = "session", feature = "rate-limit"))]
  pub(crate) fn invoke<T: FromRedisValue + Send + 'static>(
    &self,
    script: &'static Script,
    keys: Vec<String>,
    args: Vec<String>,
  ) -> Effect<T, RedisError> {
    let mut conn = self.connection();
    from_async(move || async move {
      let mut invocation = script.prepare_invoke();
      for key in &keys {
        invocation.key(key);
      }
      for arg in &args {
        invocation.arg(arg);
      }
      Ok(invocation.invoke_async(&mut conn).await?)
    })
  }

  /// `PING`: succeeds when the server answers.
  pub fn ping(&self) -> Effect<(), RedisError> {
    self.query::<String>(redis::cmd("PING")).map(|_| ())
  }

  /// Every key matching the glob `pattern` (`SCAN`, so the server is never blocked), sorted.
  pub(crate) fn scan(&self, pattern: String) -> Effect<Vec<String>, RedisError> {
    let mut conn = self.connection();
    from_async(move || async move {
      let mut keys = Vec::new();
      let mut cursor = 0_u64;
      loop {
        let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
          .arg(cursor)
          .arg("MATCH")
          .arg(&pattern)
          .arg("COUNT")
          .arg(500)
          .query_async(&mut conn)
          .await?;
        keys.extend(batch);
        if next == 0 {
          break;
        }
        cursor = next;
      }
      // SCAN may return a key more than once.
      keys.sort();
      keys.dedup();
      Ok(keys)
    })
  }

  /// Delete every key matching the glob `pattern`; how many were deleted.
  pub(crate) fn delete_matching(&self, pattern: String) -> Effect<u64, RedisError> {
    let this = self.clone();
    self.scan(pattern).flat_map(move |keys| {
      let mut conn = this.connection();
      from_async(move || async move {
        let mut deleted = 0_u64;
        for chunk in keys.chunks(500) {
          deleted += redis::cmd("UNLINK")
            .arg(chunk)
            .query_async::<u64>(&mut conn)
            .await?;
        }
        Ok(deleted)
      })
    })
  }
}

/// `key` in `namespace` (`namespace:key`; `key` itself when `namespace` is empty).
pub(crate) fn namespaced(namespace: &str, key: &str) -> String {
  if namespace.is_empty() {
    key.to_owned()
  } else {
    format!("{namespace}:{key}")
  }
}

/// The glob matching every key of `namespace` (its glob characters escaped): `*`, every key of
/// the database, for the empty namespace, so a store's `clear()` refuses that one (M17).
pub(crate) fn namespace_glob(namespace: &str) -> String {
  let mut glob = String::with_capacity(namespace.len() + 2);
  for c in namespace.chars() {
    if matches!(c, '*' | '?' | '[' | ']' | '\\') {
      glob.push('\\');
    }
    glob.push(c);
  }
  if !namespace.is_empty() {
    glob.push(':');
  }
  glob.push('*');
  glob
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn config_builder_and_redacted_debug() {
    let config = RedisConfig::new("redis://app:s3cret@cache.internal:6380/2")
      .pool_size(0)
      .connection_timeout(Duration::from_secs(2))
      .response_timeout(Duration::from_millis(500))
      .reconnect_retries(1);
    assert_eq!(config.url(), "redis://app:s3cret@cache.internal:6380/2");
    let shown = format!("{config:?}");
    assert!(
      shown.contains("redis://app:***@cache.internal:6380/2"),
      "{shown}"
    );
    assert!(!shown.contains("s3cret"), "{shown}");
    assert!(shown.contains("pool_size: 1"), "{shown}");
    let manager = config.manager_config();
    assert_eq!(manager.connection_timeout(), Some(Duration::from_secs(2)));
    assert_eq!(manager.response_timeout(), Some(Duration::from_millis(500)));
    assert_eq!(manager.number_of_retries(), 1);
    assert_eq!(RedisConfig::default().url(), RedisConfig::DEFAULT_URL);
    assert_eq!(redacted("redis://:pw@h"), "redis://:***@h");
    assert_eq!(redacted("redis://h:1"), "redis://h:1");
    assert_eq!(redacted("not a url"), "not a url");
  }

  #[test]
  fn keys_are_namespaced_and_globs_escaped() {
    assert_eq!(namespaced("users", "42"), "users:42");
    assert_eq!(namespaced("", "42"), "42");
    assert_eq!(namespace_glob("a*b?[c]\\d"), "a\\*b\\?\\[c\\]\\\\d:*");
    assert_eq!(namespace_glob(""), "*");
  }
}
