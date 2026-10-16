//! Redis for id_effect services, over redis-rs (release spec 0.9.0, decision D6).
//!
//! - [`RedisClient`]: a pool of reconnecting, multiplexed connections; the capability
//!   ([`RedisEnv`]) that [`RedisClient::layer`] ([`RedisLayer`]) builds from a [`RedisConfig`].
//!   Raw commands go through [`query`](RedisClient::query).
//! - Typed stores, values as `serde` JSON: [`RedisKv`] (key → value, with time-to-live,
//!   set-if-absent and counters), [`RedisHash`] (key → field → value) and [`RedisJson`]
//!   (documents read and updated at a JSONPath, on the RedisJSON module of Redis 8).
//! - [`RedisCacheBackend`]: Redis behind the core [`Cache`](id_effect::Cache) API
//!   (`Cache::with_backend`), so the same cache code runs in process or shared across
//!   instances.
//! - Pub/sub: [`publish`](RedisClient::publish), and [`subscribe`](RedisClient::subscribe) /
//!   [`psubscribe`](RedisClient::psubscribe) as an `id_effect` [`Stream`](id_effect::Stream) of
//!   [`RedisMessage`]s.
//! - Feature `session`: `RedisSessionStore`, id_effect_auth's `SessionStore` in Redis.
//! - Feature `rate-limit`: `RedisRateLimitStore`, id_effect_axum's `RateLimitStore`, so a rate
//!   limit holds across every instance.
//!
//! Every operation is a lazy [`Effect`](id_effect::Effect) that fails with [`RedisError`]; run
//! it on a tokio runtime (`id_effect_tokio::TokioRuntime`, or an axum server's).
//!
//! ```no_run
//! use id_effect::{Effect, need};
//! use id_effect_redis::{RedisClient, RedisConfig, RedisEnv, RedisError};
//!
//! // Count visits per page.
//! fn visit(page: &'static str) -> Effect<i64, RedisError, RedisEnv> {
//!   need::<RedisClient>().flat_map(move |redis| redis.kv::<i64>("visits").incr_by(page, 1).narrow())
//! }
//!
//! let program = visit("/").provide_layer(RedisClient::layer(RedisConfig::default()));
//! # let _ = program;
//! ```
//!
//! Tier `integration` (ADR 0010 Amendment 5); `platform` once the Ledger reference app under
//! `examples/reference/` depends on it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod cache;
mod client;
mod error;
mod pubsub;
#[cfg(feature = "rate-limit")]
mod rate_limit;
#[cfg(feature = "session")]
mod session;
mod store;

pub use cache::RedisCacheBackend;
pub use client::{RedisClient, RedisConfig, RedisEnv, RedisLayer};
pub use error::RedisError;
pub use pubsub::RedisMessage;
#[cfg(feature = "rate-limit")]
pub use rate_limit::RedisRateLimitStore;
#[cfg(feature = "session")]
pub use session::{RedisSessionStore, redis_session_store_layer};
pub use store::{RedisHash, RedisJson, RedisKv};

/// The redis-rs crate this one is built on (for [`RedisClient::query`] commands and
/// [`RedisClient::connection`]).
pub use redis;

/// Compiles the README's code block as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
