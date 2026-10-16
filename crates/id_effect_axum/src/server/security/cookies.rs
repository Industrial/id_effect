//! Cookie defaults (`HttpOnly`, `Secure`, `SameSite=Lax`, `Path=/`) and signed/private jars
//! ([`cookie`] 0.18), keyed by a [`CookieKey`] provided as an effect [`Layer`].

use std::convert::Infallible;
use std::fmt;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponseParts, ResponseParts};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use cookie::{CookieJar, Key};
use id_effect::{HCons, HNil, Layer, hlist};
use id_effect_config::{Config, ConfigEnv, ConfigError};

pub use cookie::{Cookie, SameSite};

use super::SecurityError;

/// Config key read by [`CookieKey::config`]: standard base64 of at least
/// [`COOKIE_KEY_MIN_LEN`] bytes of random key material.
pub const COOKIE_KEY_ENV: &str = "COOKIE_KEY";

/// Minimum cookie key material, in bytes (32 for signing, 32 for encryption).
pub const COOKIE_KEY_MIN_LEN: usize = 64;

/// Signing and encryption key for the CSRF cookie and the [`Cookies`] signed/private jars.
///
/// Provide it to the program as an effect layer — [`CookieKey::layer`] (from config) or
/// [`CookieKey::layer_from`] — and hand it to
/// [`SecurityLayer::recommended`](super::SecurityLayer::recommended). `Debug` never prints the
/// key.
#[derive(Clone)]
pub struct CookieKey(Key);

impl CookieKey {
  /// A key from at least [`COOKIE_KEY_MIN_LEN`] bytes of key material (extra bytes are ignored).
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, SecurityError> {
    Key::try_from(bytes)
      .map(Self)
      .map_err(|_| SecurityError::CookieKeyTooShort { len: bytes.len() })
  }

  /// A key from standard base64 text (as stored in [`COOKIE_KEY_ENV`]).
  pub fn from_base64(encoded: &str) -> Result<Self, SecurityError> {
    let bytes = STANDARD
      .decode(encoded.trim())
      .map_err(|_| SecurityError::CookieKeyEncoding)?;
    Self::from_bytes(&bytes)
  }

  /// A fresh random key. Cookies signed with it do not survive a restart; use
  /// [`CookieKey::layer`] for a stable key in production.
  pub fn generate() -> Self {
    let mut bytes = [0u8; COOKIE_KEY_MIN_LEN];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    Self(Key::from(&bytes))
  }

  /// The config descriptor: [`COOKIE_KEY_ENV`] as a secret, decoded and length-checked (a bad
  /// key is a [`ConfigError::Invalid`] that does not echo the value).
  #[allow(clippy::result_large_err)]
  pub fn config() -> Config<CookieKey> {
    Config::string(COOKIE_KEY_ENV)
      .secret()
      .map_attempt(|secret| {
        Self::from_base64(secret.expose()).map_err(|e| ConfigError::Invalid {
          path: COOKIE_KEY_ENV.into(),
          value: "<redacted>".into(),
          reason: e.to_string(),
        })
      })
  }

  /// A layer providing [`CookieKey`] from the config provider capability (feed it a provider
  /// layer with `provide_to`, as for [`HostConfig::layer`](crate::server::HostConfig::layer)).
  pub fn layer<E>() -> Layer<HCons<CookieKey, HNil>, E, ConfigEnv>
  where
    E: From<ConfigError> + Send + 'static,
  {
    Self::config().layer()
  }

  /// A layer providing an existing `key` (tests, or keys from a secret manager).
  pub fn layer_from<E>(key: CookieKey) -> Layer<HCons<CookieKey, HNil>, E>
  where
    E: Send + 'static,
  {
    Layer::succeed(hlist![key])
  }

  pub(crate) fn key(&self) -> &Key {
    &self.0
  }
}

impl fmt::Debug for CookieKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("CookieKey(<redacted>)")
  }
}

/// Attributes applied to cookies added through [`Cookies`] when the cookie does not set them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookieDefaults {
  http_only: bool,
  secure: bool,
  same_site: SameSite,
  path: String,
}

impl Default for CookieDefaults {
  fn default() -> Self {
    Self::recommended()
  }
}

impl CookieDefaults {
  /// `HttpOnly`, `Secure`, `SameSite=Lax`, `Path=/`.
  pub fn recommended() -> Self {
    Self {
      http_only: true,
      secure: true,
      same_site: SameSite::Lax,
      path: "/".into(),
    }
  }

  /// Default `HttpOnly` flag.
  pub fn http_only(mut self, on: bool) -> Self {
    self.http_only = on;
    self
  }

  /// Default `Secure` flag (turn off only for plain-HTTP local development).
  pub fn secure(mut self, on: bool) -> Self {
    self.secure = on;
    self
  }

  /// Default `SameSite` attribute.
  pub fn same_site(mut self, same_site: SameSite) -> Self {
    self.same_site = same_site;
    self
  }

  /// Default `Path` attribute.
  pub fn path(mut self, path: impl Into<String>) -> Self {
    self.path = path.into();
    self
  }

  /// Fill in every attribute `cookie` leaves unset.
  pub fn apply(&self, cookie: &mut Cookie<'static>) {
    if cookie.http_only().is_none() {
      cookie.set_http_only(self.http_only);
    }
    if cookie.secure().is_none() {
      cookie.set_secure(self.secure);
    }
    if cookie.same_site().is_none() {
      cookie.set_same_site(self.same_site);
    }
    if cookie.path().is_none() {
      cookie.set_path(self.path.clone());
    }
  }
}

/// The request's cookies as a jar, with plain, signed and private access.
///
/// As an extractor it picks up the [`CookieKey`] and [`CookieDefaults`] installed by
/// [`SecurityLayer`](super::SecurityLayer); return it from the handler (it is
/// [`IntoResponseParts`]) to send the cookies added or removed.
#[derive(Debug)]
pub struct Cookies {
  jar: CookieJar,
  key: Option<CookieKey>,
  defaults: CookieDefaults,
}

impl Cookies {
  /// Cookies parsed from `Cookie` headers, with recommended defaults and no key.
  pub fn from_headers(headers: &HeaderMap) -> Self {
    Self {
      jar: request_jar(headers),
      key: None,
      defaults: CookieDefaults::recommended(),
    }
  }

  /// Use `key` for signed and private cookies.
  pub fn with_key(mut self, key: CookieKey) -> Self {
    self.key = Some(key);
    self
  }

  /// Use `defaults` for added cookies.
  pub fn with_defaults(mut self, defaults: CookieDefaults) -> Self {
    self.defaults = defaults;
    self
  }

  /// A plain (unverified) cookie.
  pub fn get(&self, name: &str) -> Option<&Cookie<'static>> {
    self.jar.get(name)
  }

  /// A signed cookie whose signature verifies (`None` if missing, forged or no key).
  pub fn get_signed(&self, name: &str) -> Option<Cookie<'static>> {
    let key = self.key.as_ref()?;
    self.jar.signed(key.key()).get(name)
  }

  /// A private cookie that decrypts and authenticates (`None` if missing, forged or no key).
  pub fn get_private(&self, name: &str) -> Option<Cookie<'static>> {
    let key = self.key.as_ref()?;
    self.jar.private(key.key()).get(name)
  }

  /// Add a plain cookie, filling unset attributes from the defaults.
  pub fn add(&mut self, mut cookie: Cookie<'static>) {
    self.defaults.apply(&mut cookie);
    self.jar.add(cookie);
  }

  /// Add a signed cookie (readable, tamper-evident).
  pub fn add_signed(&mut self, mut cookie: Cookie<'static>) -> Result<(), SecurityError> {
    let key = self.key.as_ref().ok_or(SecurityError::MissingCookieKey)?;
    self.defaults.apply(&mut cookie);
    self.jar.signed_mut(key.key()).add(cookie);
    Ok(())
  }

  /// Add a private cookie (encrypted and authenticated).
  pub fn add_private(&mut self, mut cookie: Cookie<'static>) -> Result<(), SecurityError> {
    let key = self.key.as_ref().ok_or(SecurityError::MissingCookieKey)?;
    self.defaults.apply(&mut cookie);
    self.jar.private_mut(key.key()).add(cookie);
    Ok(())
  }

  /// Remove the cookie `name` (sends an expired cookie on the default path).
  pub fn remove(&mut self, name: impl Into<String>) {
    let removal = Cookie::build((name.into(), "")).path(self.defaults.path.clone());
    self.jar.remove(removal);
  }

  /// `Set-Cookie` values for every cookie added or removed.
  pub fn set_cookie_headers(&self) -> Vec<HeaderValue> {
    self
      .jar
      .delta()
      .filter_map(|cookie| HeaderValue::from_str(&cookie.to_string()).ok())
      .collect()
  }
}

impl<S> FromRequestParts<S> for Cookies
where
  S: Send + Sync,
{
  type Rejection = Infallible;

  async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
    let mut cookies = Self::from_headers(&parts.headers);
    cookies.key = parts.extensions.get::<CookieKey>().cloned();
    if let Some(defaults) = parts.extensions.get::<CookieDefaults>() {
      cookies.defaults = defaults.clone();
    }
    Ok(cookies)
  }
}

impl IntoResponseParts for Cookies {
  type Error = Infallible;

  fn into_response_parts(self, mut res: ResponseParts) -> Result<ResponseParts, Self::Error> {
    for value in self.set_cookie_headers() {
      res.headers_mut().append(header::SET_COOKIE, value);
    }
    Ok(res)
  }
}

/// Every cookie in the request's `Cookie` headers, as originals (unparseable pairs are skipped).
pub(crate) fn request_jar(headers: &HeaderMap) -> CookieJar {
  let mut jar = CookieJar::new();
  for value in headers.get_all(header::COOKIE) {
    let Ok(text) = value.to_str() else { continue };
    for cookie in Cookie::split_parse(text).flatten() {
      jar.add_original(cookie.into_owned());
    }
  }
  jar
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect_config::MapConfigProvider;

  #[test]
  fn cookie_key_rejects_short_material() {
    assert_eq!(
      CookieKey::from_bytes(&[7; 10]).unwrap_err(),
      SecurityError::CookieKeyTooShort { len: 10 }
    );
    assert!(CookieKey::from_bytes(&[7; COOKIE_KEY_MIN_LEN]).is_ok());
    assert_eq!(
      CookieKey::from_base64("not base64!").unwrap_err(),
      SecurityError::CookieKeyEncoding
    );
  }

  #[test]
  fn cookie_key_debug_is_redacted() {
    assert_eq!(
      format!("{:?}", CookieKey::generate()),
      "CookieKey(<redacted>)"
    );
  }

  #[test]
  fn cookie_key_config_reports_bad_key_without_echoing_it() {
    let p = MapConfigProvider::from_pairs([(COOKIE_KEY_ENV, "c2hvcnQ=")]);
    match CookieKey::config().load(&p) {
      Err(ConfigError::Invalid { value, .. }) => assert_eq!(value, "<redacted>"),
      other => panic!("expected invalid, got {:?}", other.map(|_| ())),
    }
    let good = STANDARD.encode([1u8; COOKIE_KEY_MIN_LEN]);
    let p = MapConfigProvider::from_pairs([(COOKIE_KEY_ENV, good.as_str())]);
    assert!(CookieKey::config().load(&p).is_ok());
  }

  #[test]
  fn cookie_defaults_fill_only_unset_attributes() {
    let mut cookie = Cookie::build(("a", "1"))
      .same_site(SameSite::Strict)
      .build();
    CookieDefaults::recommended().apply(&mut cookie);
    assert_eq!(cookie.http_only(), Some(true));
    assert_eq!(cookie.secure(), Some(true));
    assert_eq!(cookie.same_site(), Some(SameSite::Strict));
    assert_eq!(cookie.path(), Some("/"));
  }

  #[test]
  fn cookie_signed_round_trip_and_missing_key() {
    let key = CookieKey::generate();
    let mut out = Cookies::from_headers(&HeaderMap::new()).with_key(key.clone());
    out.add_signed(Cookie::new("sid", "42")).unwrap();
    let set = out.set_cookie_headers();
    assert_eq!(set.len(), 1);
    let pair = set[0]
      .to_str()
      .unwrap()
      .split(';')
      .next()
      .unwrap()
      .to_owned();
    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, HeaderValue::from_str(&pair).unwrap());
    let back = Cookies::from_headers(&headers).with_key(key);
    assert_eq!(back.get_signed("sid").unwrap().value(), "42");
    let mut keyless = Cookies::from_headers(&headers);
    assert!(keyless.get_signed("sid").is_none());
    assert_eq!(
      keyless.add_private(Cookie::new("x", "y")),
      Err(SecurityError::MissingCookieKey)
    );
  }
}
