//! [`JwksClient`] (feature `jwks`): an issuer's JSON Web Key Set, fetched over HTTP and cached,
//! refetched when a token names a key it does not know.
//!
//! Identity providers rotate signing keys by publishing the new key in their JWKS document
//! (`/.well-known/jwks.json`, `…/protocol/openid-connect/certs`) and then signing with it. The
//! client therefore:
//!
//! - fetches the set on first use and serves keys from memory afterwards;
//! - refetches when a token's `kid` is not in the cache (**rotation**) and when the cache is
//!   older than [`max_age`](JwksClient::max_age) (default 15 minutes; removed keys disappear);
//! - refetches at most once per [`min_refresh_interval`](JwksClient::min_refresh_interval)
//!   (default 30 s), so tokens with made-up `kid`s cannot make it hammer the provider; concurrent
//!   requests share one fetch;
//! - keeps serving the cached keys when a refetch fails, and answers
//!   [`AuthError::Unavailable`] only when it has no usable key.
//!
//! Keys of types jsonwebtoken does not support and keys marked `"use": "enc"` are skipped.
//! Requests go through reqwest with rustls and need a tokio runtime (an axum server has one).
//!
//! The key set decides which tokens are trusted, so the fetch is strict: the URL must be
//! `https` (plain `http` only to a loopback host, for tests, or after
//! [`allow_http_dangerous`](JwksClient::allow_http_dangerous)), redirects are not followed, and
//! a body larger than [`MAX_BODY_BYTES`] (1 MiB) is refused while it is read.
//!
//! ```no_run
//! use std::time::Duration;
//! use id_effect_auth::{JwksClient, JwtVerifier};
//!
//! let jwks = JwksClient::new("https://id.example.com/realms/ledger/protocol/openid-connect/certs")
//!   .min_refresh_interval(Duration::from_secs(60));
//! let verifier = JwtVerifier::jwks(jwks)
//!   .issuer("https://id.example.com/realms/ledger")
//!   .audience("ledger-api");
//! # let _ = verifier;
//! ```

use std::fmt;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use id_effect::{Effect, from_async};
use reqwest::Client;
use serde_json::Value;

use crate::error::AuthError;
use crate::jwt::{Algorithm, Jwk, JwtKey, select_key};

/// Default [`JwksClient::min_refresh_interval`].
pub const DEFAULT_MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
/// Default [`JwksClient::max_age`].
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(15 * 60);
/// Default [`JwksClient::timeout`].
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest JWKS document a [`JwksClient`] reads, in bytes.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
struct Config {
  url: String,
  min_refresh: Duration,
  max_age: Duration,
  timeout: Duration,
  allow_http: bool,
}

#[derive(Default)]
struct Cache {
  keys: Arc<[JwtKey]>,
  fetched_at: Option<Instant>,
  attempted_at: Option<Instant>,
  last_error: Option<String>,
}

#[derive(Default)]
struct Shared {
  cache: Mutex<Cache>,
  /// Held while a fetch runs, so concurrent misses share it.
  refreshing: tokio::sync::Mutex<()>,
  http: OnceLock<Client>,
  fetches: AtomicU64,
}

/// Fetches, caches and rotates an issuer's JSON Web Key Set. Cheap to clone; clones share the
/// cache. See the [module documentation](self).
#[derive(Clone)]
pub struct JwksClient {
  config: Arc<Config>,
  shared: Arc<Shared>,
}

impl fmt::Debug for JwksClient {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("JwksClient")
      .field("url", &self.config.url)
      .field("keys", &self.cache().keys.len())
      .field("fetches", &self.fetch_count())
      .finish()
  }
}

impl JwksClient {
  /// A client for the JWKS document at `url`, with the default intervals.
  pub fn new(url: impl Into<String>) -> Self {
    Self {
      config: Arc::new(Config {
        url: url.into(),
        min_refresh: DEFAULT_MIN_REFRESH_INTERVAL,
        max_age: DEFAULT_MAX_AGE,
        timeout: DEFAULT_TIMEOUT,
        allow_http: false,
      }),
      shared: Arc::new(Shared::default()),
    }
  }

  /// Fetch over plain `http` from any host, not only loopback ones.
  ///
  /// **Dangerous**: anyone on the network path can then swap the key set and have the verifier
  /// accept tokens they signed. Only for a provider reached over a network that is itself
  /// trusted and encrypted (a service mesh with mTLS, for example).
  #[must_use]
  pub fn allow_http_dangerous(mut self) -> Self {
    Arc::make_mut(&mut self.config).allow_http = true;
    self
  }

  /// Refetch at most once per `interval` (rate limit against unknown `kid`s).
  #[must_use]
  pub fn min_refresh_interval(mut self, interval: Duration) -> Self {
    Arc::make_mut(&mut self.config).min_refresh = interval;
    self
  }

  /// Refetch a set older than `max_age` on the next lookup.
  #[must_use]
  pub fn max_age(mut self, max_age: Duration) -> Self {
    Arc::make_mut(&mut self.config).max_age = max_age;
    self
  }

  /// Give up on a fetch after `timeout`.
  #[must_use]
  pub fn timeout(mut self, timeout: Duration) -> Self {
    Arc::make_mut(&mut self.config).timeout = timeout;
    self
  }

  /// The JWKS document's URL.
  #[inline]
  pub fn url(&self) -> &str {
    &self.config.url
  }

  /// How many fetches were attempted (for tests and metrics).
  pub fn fetch_count(&self) -> u64 {
    self.shared.fetches.load(Ordering::SeqCst)
  }

  /// The keys currently cached.
  pub fn cached_keys(&self) -> Vec<JwtKey> {
    self.cache().keys.to_vec()
  }

  fn cache(&self) -> MutexGuard<'_, Cache> {
    self
      .shared
      .cache
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
  }

  /// Fetch the set now (ignoring the rate limit); how many usable keys it holds.
  pub fn refresh(&self) -> Effect<usize, AuthError> {
    let this = self.clone();
    from_async(move || async move {
      let _running = this.shared.refreshing.lock().await;
      this.fetch_and_store().await
    })
  }

  /// The key for a token signed with `alg` and naming `kid`, refetching the set when the key is
  /// unknown or the cache is old (see the [module documentation](self)).
  pub fn key(&self, kid: Option<String>, alg: Algorithm) -> Effect<JwtKey, AuthError> {
    let this = self.clone();
    from_async(move || async move { this.lookup(kid.as_deref(), alg).await })
  }

  async fn lookup(&self, kid: Option<&str>, alg: Algorithm) -> Result<JwtKey, AuthError> {
    if let Some(key) = self.fresh_key(kid, alg) {
      return Ok(key);
    }
    let _running = self.shared.refreshing.lock().await;
    // Another request may have refetched while this one waited.
    if let Some(key) = self.fresh_key(kid, alg) {
      return Ok(key);
    }
    let fetched = if self.may_fetch() {
      Some(self.fetch_and_store().await)
    } else {
      None
    };
    let cache = self.cache();
    select_key(&cache.keys, kid, alg).map_err(|unknown| match (fetched, &cache.last_error) {
      (Some(Err(e)), _) => e,
      (None, Some(e)) if cache.keys.is_empty() => AuthError::Unavailable(e.clone()),
      _ => unknown,
    })
  }

  /// A cached key, when the cache is younger than `max_age`.
  fn fresh_key(&self, kid: Option<&str>, alg: Algorithm) -> Option<JwtKey> {
    let cache = self.cache();
    let fresh = cache
      .fetched_at
      .is_some_and(|t| t.elapsed() < self.config.max_age);
    fresh.then(|| select_key(&cache.keys, kid, alg).ok())?
  }

  /// Whether the rate limit allows a fetch now; if so, records the attempt.
  fn may_fetch(&self) -> bool {
    let mut cache = self.cache();
    let allowed = cache
      .attempted_at
      .is_none_or(|t| t.elapsed() >= self.config.min_refresh);
    if allowed {
      cache.attempted_at = Some(Instant::now());
    }
    allowed
  }

  async fn fetch_and_store(&self) -> Result<usize, AuthError> {
    self.shared.fetches.fetch_add(1, Ordering::SeqCst);
    let result = self.fetch().await;
    let mut cache = self.cache();
    cache.attempted_at = Some(Instant::now());
    match result {
      Ok(keys) => {
        let n = keys.len();
        cache.keys = keys.into();
        cache.fetched_at = Some(Instant::now());
        cache.last_error = None;
        Ok(n)
      }
      Err(e) => {
        cache.last_error = Some(e.to_string());
        Err(e)
      }
    }
  }

  async fn fetch(&self) -> Result<Vec<JwtKey>, AuthError> {
    let unavailable = |what: String| AuthError::Unavailable(format!("JWKS {what}"));
    let url = checked_url(&self.config.url, self.config.allow_http)?;
    let client = match self.shared.http.get() {
      Some(c) => c.clone(),
      None => {
        let built = Client::builder()
          .timeout(self.config.timeout)
          .redirect(reqwest::redirect::Policy::none())
          .build()
          .map_err(|e| unavailable(format!("client: {e}")))?;
        self.shared.http.get_or_init(|| built).clone()
      }
    };
    let mut response = client
      .get(url)
      .header(reqwest::header::ACCEPT, "application/json")
      .send()
      .await
      .map_err(|e| unavailable(format!("request failed: {e}")))?;
    let status = response.status();
    if !status.is_success() {
      return Err(unavailable(format!("endpoint answered {status}")));
    }
    let too_large = || unavailable(format!("body is larger than {MAX_BODY_BYTES} bytes"));
    if response
      .content_length()
      .is_some_and(|n| n > MAX_BODY_BYTES as u64)
    {
      return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
      .chunk()
      .await
      .map_err(|e| unavailable(format!("body: {e}")))?
    {
      if body.len() + chunk.len() > MAX_BODY_BYTES {
        return Err(too_large());
      }
      body.extend_from_slice(&chunk);
    }
    parse_jwks(&body).map_err(unavailable)
  }
}

/// `url` parsed, when it is `https`, or `http` to a loopback host (or any host with
/// `allow_http`); [`AuthError::Config`] otherwise.
fn checked_url(url: &str, allow_http: bool) -> Result<reqwest::Url, AuthError> {
  let config = |why: &str| AuthError::Config(format!("JWKS URL {url:?} {why}"));
  let parsed = reqwest::Url::parse(url).map_err(|_| config("is not a URL"))?;
  let loopback = parsed.host_str().is_some_and(|host| {
    host.eq_ignore_ascii_case("localhost")
      || host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
  });
  match parsed.scheme() {
    "https" => Ok(parsed),
    "http" if loopback || allow_http => Ok(parsed),
    "http" => Err(config(
      "must be https (plain http only to a loopback host, or after allow_http_dangerous())",
    )),
    _ => Err(config("must be https")),
  }
}

/// The usable signing keys of a JWKS document.
fn parse_jwks(body: &[u8]) -> Result<Vec<JwtKey>, String> {
  let doc: Value = serde_json::from_slice(body).map_err(|e| format!("is not JSON: {e}"))?;
  let keys = doc
    .get("keys")
    .and_then(Value::as_array)
    .ok_or_else(|| "has no \"keys\" array".to_owned())?;
  Ok(
    keys
      .iter()
      .filter(|k| k.get("use").and_then(Value::as_str) != Some("enc"))
      .filter_map(|k| serde_json::from_value::<Jwk>(k.clone()).ok())
      .filter_map(|jwk| JwtKey::from_jwk(&jwk).ok())
      .collect(),
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn parse_skips_unusable_keys() {
    let doc = json!({"keys": [
      {"kty": "oct", "k": "c2VjcmV0", "kid": "h", "alg": "HS256"},
      {"kty": "oct", "k": "c2VjcmV0", "kid": "enc", "use": "enc"},
      {"kty": "weird", "kid": "w"},
      {"kty": "oct", "k": "c2VjcmV0", "kid": "bad-alg", "alg": "RSA-OAEP"}
    ]});
    let keys = parse_jwks(doc.to_string().as_bytes()).unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].kid(), Some("h"));
    assert!(parse_jwks(b"nope").unwrap_err().contains("not JSON"));
    assert!(parse_jwks(b"{}").unwrap_err().contains("keys"));
  }

  #[test]
  fn m15_jwks_requires_https() {
    for ok in [
      "https://id.example.com/certs",
      "http://localhost:8080/certs",
      "http://LOCALHOST/certs",
      "http://127.0.0.1:9/certs",
      "http://127.8.9.10/certs",
      "http://[::1]:8080/certs",
    ] {
      assert!(checked_url(ok, false).is_ok(), "{ok}");
    }
    for bad in [
      "http://id.example.com/certs",
      "http://10.0.0.1/certs",
      "http://localhost.example.com/certs",
      "ftp://id.example.com/certs",
      "file:///etc/jwks.json",
      "not a url",
    ] {
      assert!(
        matches!(checked_url(bad, false), Err(AuthError::Config(_))),
        "{bad}"
      );
    }
    assert!(checked_url("http://id.example.com/certs", true).is_ok());
    assert!(checked_url("ftp://id.example.com/certs", true).is_err());
    let lax = JwksClient::new("http://keycloak:8080/certs").allow_http_dangerous();
    assert!(lax.config.allow_http);
  }

  #[test]
  fn builder_and_debug() {
    let c = JwksClient::new("http://localhost/jwks")
      .min_refresh_interval(Duration::from_secs(1))
      .max_age(Duration::from_secs(2))
      .timeout(Duration::from_secs(3));
    assert_eq!(c.url(), "http://localhost/jwks");
    assert_eq!(
      (c.config.min_refresh, c.config.max_age, c.config.timeout),
      (
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(3)
      )
    );
    assert_eq!(c.fetch_count(), 0);
    assert!(c.cached_keys().is_empty());
    assert!(format!("{c:?}").contains("localhost"));
  }
}
