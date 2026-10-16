//! [`RedisRateLimitStore`]: id_effect_axum's [`RateLimitStore`] in Redis (feature
//! `rate-limit`).

use std::fmt;
use std::sync::LazyLock;

use id_effect::{Effect, HNil, die, fail, succeed};
use id_effect_axum::middleware::{RateLimitError, RateLimitStore};
use redis::Script;

use crate::client::RedisClient;

/// Take one token. KEYS: bucket. ARGV: rate (tokens per second), burst, time-to-live (ms).
/// The bucket refills on the server's clock, so every instance sees the same time.
static ACQUIRE: LazyLock<Script> = LazyLock::new(|| {
  Script::new(
    r"
local rate = tonumber(ARGV[1])
local burst = tonumber(ARGV[2])
local now = redis.call('TIME')
local b = redis.call('HMGET', KEYS[1], 'tokens', 's', 'us')
local tokens = tonumber(b[1])
if tokens == nil then
  tokens = burst
else
  local elapsed = (tonumber(now[1]) - tonumber(b[2])) * 1000000 + (tonumber(now[2]) - tonumber(b[3]))
  if elapsed > 0 then tokens = math.min(burst, tokens + elapsed * rate / 1000000) end
end
local ok = 0
if tokens >= 1 then
  tokens = tokens - 1
  ok = 1
end
redis.call('HSET', KEYS[1], 'tokens', tostring(tokens), 's', now[1], 'us', now[2])
redis.call('PEXPIRE', KEYS[1], ARGV[3])
return ok
",
  )
});

/// Token buckets in Redis, shared by every instance of a service behind
/// [`RateLimitLayer`](id_effect_axum::middleware::RateLimitLayer): `rate` tokens per second up
/// to `burst` per key (each bucket starts full), as `MemoryRateLimitStore` does in process.
///
/// A bucket is a hash under `{prefix}:{key}`, updated by one Lua script on the server's clock,
/// and expires once it would be full again. When Redis fails, the check dies and the layer lets
/// the request through (fail open), as for any store.
///
/// ```no_run
/// use axum::{Router, routing::get};
/// use id_effect_axum::middleware::RateLimitLayer;
/// use id_effect_redis::{RedisClient, RedisRateLimitStore};
///
/// fn app(redis: RedisClient) -> Router {
///   Router::new()
///     .route("/api", get(|| async { "ok" }))
///     .layer(RateLimitLayer::new(RedisRateLimitStore::new(redis, 10.0, 20.0)))
/// }
/// ```
#[derive(Clone)]
pub struct RedisRateLimitStore {
  client: RedisClient,
  prefix: String,
  rate: f64,
  burst: f64,
}

impl fmt::Debug for RedisRateLimitStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RedisRateLimitStore")
      .field("prefix", &self.prefix)
      .field("rate", &self.rate)
      .field("burst", &self.burst)
      .finish_non_exhaustive()
  }
}

impl RedisRateLimitStore {
  /// Default key prefix.
  pub const DEFAULT_PREFIX: &'static str = "id_effect:rate-limit";

  /// Buckets refilling `rate` tokens per second up to `burst` tokens.
  pub fn new(client: RedisClient, rate: f64, burst: f64) -> Self {
    Self {
      client,
      prefix: Self::DEFAULT_PREFIX.to_owned(),
      rate,
      burst,
    }
  }

  /// Keep the buckets under `prefix` instead (one per limit, when a service has several).
  #[must_use]
  pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
    self.prefix = prefix.into();
    self
  }

  /// How long an untouched bucket is kept: until it would be full again (a day when it never
  /// refills), plus a second.
  fn ttl_millis(&self) -> u64 {
    let full = if self.rate > 0.0 {
      (self.burst.max(1.0) / self.rate * 1000.0).ceil()
    } else {
      86_400_000.0
    };
    // Saturating float-to-int conversion; NaN becomes 0.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let full = full.min(86_400_000.0) as u64;
    full + 1000
  }
}

impl RateLimitStore for RedisRateLimitStore {
  fn acquire(&self, key: &str) -> Effect<(), RateLimitError, HNil> {
    let args = vec![
      self.rate.to_string(),
      self.burst.to_string(),
      self.ttl_millis().to_string(),
    ];
    self
      .client
      .invoke::<i64>(&ACQUIRE, vec![format!("{}:{key}", self.prefix)], args)
      .catch(|e| die(format!("rate limit store: {e}")))
      .flat_map(|taken| {
        if taken == 1 {
          succeed(())
        } else {
          fail(RateLimitError)
        }
      })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn buckets_expire_once_full_again() {
    let client = crate::RedisClient::connect_lazy(crate::RedisConfig::new("redis://127.0.0.1:1"));
    let client = match id_effect_tokio::TokioRuntime::current()
      .unwrap()
      .run(client)
      .await
    {
      id_effect::Exit::Success(c) => c,
      other => panic!("{other:?}"),
    };
    let store = RedisRateLimitStore::new(client.clone(), 4.0, 2.0).with_prefix("p");
    assert_eq!(store.ttl_millis(), 1500);
    assert_eq!(
      RedisRateLimitStore::new(client.clone(), 0.0, 2.0).ttl_millis(),
      86_401_000
    );
    assert_eq!(
      RedisRateLimitStore::new(client, 1e-9, 2.0).ttl_millis(),
      86_401_000
    );
    assert!(format!("{store:?}").contains("prefix: \"p\""));
  }
}
