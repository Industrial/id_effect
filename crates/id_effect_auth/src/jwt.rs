//! JSON Web Tokens (feature `jwt`): [`JwtVerifier`] turns a bearer token into an
//! [`AuthPrincipal`], [`JwtIssuer`] signs first-party tokens.
//!
//! Signatures, `iss` and `aud` are checked by [jsonwebtoken]; `exp` and `nbf` are checked here,
//! against a [`WallClock`], so expiry runs in virtual time under a `TestExecutor` or an anchored
//! clock. A verifier needs:
//!
//! - **keys** — [`JwtKey`]s given up front (an HMAC secret, PEM public keys, JWKs), or with
//!   feature `jwks` a [`JwksClient`](crate::jwks::JwksClient) that fetches the issuer's key set and
//!   refetches it when a token names an unknown `kid` (key rotation). A key only verifies the
//!   algorithms of its own family, so an RSA public key can never be used as an HMAC secret;
//! - **expectations** — accepted issuers and audiences, required claims, clock leeway (60 s by
//!   default), and whether `exp` is required (yes by default). A verifier with neither an
//!   issuer nor an audience rejects every token with [`AuthError::Config`]: it would accept
//!   tokens minted for any service by anyone holding a key it trusts. Opt out with both
//!   [`dangerous_any_issuer`](JwtVerifier::dangerous_any_issuer) and
//!   [`dangerous_any_audience`](JwtVerifier::dangerous_any_audience);
//! - a **[`ClaimMapping`]** — which claims hold the subject, roles, permissions and tenant
//!   (`sub`, `roles`, `permissions` / `scope` / `scp` and `tenant` by default; dotted paths such
//!   as `realm_access.roles` reach into objects).
//!
//! HMAC secrets shorter than [`MIN_HMAC_KEY_LEN`] (32) bytes are refused (RFC 7518 §3.2): a
//! verifier with one rejects the tokens it would check and an issuer with one issues nothing,
//! both with [`AuthError::Key`].
//!
//! ```
//! use id_effect::{Cause, Exit};
//! use id_effect_auth::{AuthError, AuthPrincipal, JwtIssuer, JwtKey, JwtVerifier};
//! use id_effect_authz::Principal;
//!
//! let secret = b"a 256-bit secret for HS256 tokens";
//! let issuer = JwtIssuer::hs256(secret).issuer("https://id.example.com").audience("ledger");
//! let verifier = JwtVerifier::new([JwtKey::hmac(secret)])
//!   .issuer("https://id.example.com")
//!   .audience("ledger");
//!
//! let alice = AuthPrincipal::new("alice").with_role("accountant");
//! let Exit::Success(token) = issuer.issue(&alice).block_on() else { panic!() };
//! let Exit::Success(p) = verifier.verify(&token).block_on() else { panic!() };
//! assert_eq!((p.id(), p.roles()), ("alice", &["accountant".to_owned()][..]));
//!
//! let other = JwtVerifier::new([JwtKey::hmac(b"another 256-bit secret for HS256 tokens")])
//!   .issuer("https://id.example.com")
//!   .audience("ledger");
//! let rejected = other.verify(&token).block_on();
//! assert!(matches!(rejected, Exit::Failure(Cause::Fail(AuthError::InvalidCredentials))));
//! ```
//!
//! [jsonwebtoken]: https://docs.rs/jsonwebtoken

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use id_effect::{Effect, UtcDateTime, WallClock, fail, succeed};
use jsonwebtoken::errors::{Error as JwtLibError, ErrorKind};
use jsonwebtoken::jwk::{CommonParameters, KeyAlgorithm, PublicKeyUse};
pub use jsonwebtoken::jwk::{Jwk, JwkSet};
pub use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey};
use jsonwebtoken::{AlgorithmFamily, Header, Validation, decode, decode_header, encode};
use serde_json::{Map, Value};

use crate::error::AuthError;
use crate::principal::{AuthPrincipal, AuthScheme, split_list};

/// Default clock-skew allowance for `exp` and `nbf`.
pub const DEFAULT_LEEWAY: Duration = Duration::from_secs(60);

/// Default lifetime of an issued token.
pub const DEFAULT_TTL: Duration = Duration::from_secs(15 * 60);

/// The shortest HMAC secret a [`JwtKey`] or [`JwtIssuer`] accepts, in bytes (RFC 7518 §3.2: at
/// least the hash output, 256 bits for HS256).
pub const MIN_HMAC_KEY_LEN: usize = 32;

/// Posture id of [`JwtVerifier::dangerous_any_issuer`] (and the deprecated
/// [`JwtVerifier::any_issuer_dangerous`]).
pub const DANGEROUS_ANY_ISSUER: &str = "id_effect_auth::jwt::JwtVerifier::dangerous_any_issuer";

/// Posture id of [`JwtVerifier::dangerous_any_audience`] (and the deprecated
/// [`JwtVerifier::any_audience_dangerous`]).
pub const DANGEROUS_ANY_AUDIENCE: &str = "id_effect_auth::jwt::JwtVerifier::dangerous_any_audience";

/// [`AuthError::Key`] for an HMAC secret of `len` bytes, when it is too short.
fn check_hmac_len(len: usize) -> Result<(), AuthError> {
  if len < MIN_HMAC_KEY_LEN {
    return Err(AuthError::Key(format!(
      "an HMAC secret needs at least {MIN_HMAC_KEY_LEN} bytes, this one has {len}"
    )));
  }
  Ok(())
}

impl From<JwtLibError> for AuthError {
  fn from(e: JwtLibError) -> Self {
    match e.kind() {
      ErrorKind::InvalidSignature | ErrorKind::InvalidAlgorithm => Self::InvalidCredentials,
      ErrorKind::ExpiredSignature => Self::Expired,
      ErrorKind::ImmatureSignature => Self::NotYetValid,
      ErrorKind::InvalidIssuer => Self::InvalidClaims("iss is not accepted".into()),
      ErrorKind::InvalidAudience => Self::InvalidClaims("aud is not accepted".into()),
      ErrorKind::InvalidSubject => Self::InvalidClaims("sub is not accepted".into()),
      ErrorKind::MissingRequiredClaim(c) => Self::InvalidClaims(format!("missing claim {c}")),
      ErrorKind::InvalidClaimFormat(c) => Self::InvalidClaims(format!("claim {c} is malformed")),
      ErrorKind::InvalidToken | ErrorKind::Base64(_) | ErrorKind::Json(_) | ErrorKind::Utf8(_) => {
        Self::Malformed("not a well-formed JWT".into())
      }
      _ => Self::Key(e.to_string()),
    }
  }
}

/// A verification key: the key, the algorithms it may verify, and its `kid`.
#[derive(Clone)]
pub struct JwtKey {
  kid: Option<String>,
  algorithms: Vec<Algorithm>,
  key: DecodingKey,
}

impl fmt::Debug for JwtKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("JwtKey")
      .field("kid", &self.kid)
      .field("algorithms", &self.algorithms)
      .finish_non_exhaustive()
  }
}

impl JwtKey {
  /// `key`, verifying `algorithms` (those outside the key's family are dropped).
  pub fn new(key: DecodingKey, algorithms: impl IntoIterator<Item = Algorithm>) -> Self {
    let family = key.family();
    Self {
      kid: None,
      algorithms: algorithms
        .into_iter()
        .filter(|a| family.algorithms().contains(a))
        .collect(),
      key,
    }
  }

  /// `key`, verifying every algorithm of its family.
  fn family(key: DecodingKey) -> Self {
    let algorithms = key.family().algorithms().to_vec();
    Self::new(key, algorithms)
  }

  /// An HMAC secret (HS256, HS384, HS512). A secret shorter than [`MIN_HMAC_KEY_LEN`] bytes
  /// makes every verification with it fail with [`AuthError::Key`].
  pub fn hmac(secret: &[u8]) -> Self {
    Self::family(DecodingKey::from_secret(secret))
  }

  /// An RSA public key in PEM (RS256/384/512, PS256/384/512).
  ///
  /// # Errors
  ///
  /// [`AuthError::Key`] when `pem` is not an RSA public key.
  pub fn rsa_pem(pem: &[u8]) -> Result<Self, AuthError> {
    DecodingKey::from_rsa_pem(pem)
      .map(Self::family)
      .map_err(|e| AuthError::Key(e.to_string()))
  }

  /// An elliptic-curve public key in PEM (ES256, ES384).
  ///
  /// # Errors
  ///
  /// [`AuthError::Key`] when `pem` is not an EC public key.
  pub fn ec_pem(pem: &[u8]) -> Result<Self, AuthError> {
    DecodingKey::from_ec_pem(pem)
      .map(Self::family)
      .map_err(|e| AuthError::Key(e.to_string()))
  }

  /// An Ed25519 public key in PEM (EdDSA).
  ///
  /// # Errors
  ///
  /// [`AuthError::Key`] when `pem` is not an Ed25519 public key.
  pub fn ed_pem(pem: &[u8]) -> Result<Self, AuthError> {
    DecodingKey::from_ed_pem(pem)
      .map(Self::family)
      .map_err(|e| AuthError::Key(e.to_string()))
  }

  /// A key from a JSON Web Key: its `kid`, and its `alg` when set (else every algorithm of its
  /// family).
  ///
  /// # Errors
  ///
  /// [`AuthError::Key`] for a key type or algorithm jsonwebtoken does not support.
  pub fn from_jwk(jwk: &Jwk) -> Result<Self, AuthError> {
    let key = DecodingKey::from_jwk(jwk).map_err(|e| AuthError::Key(e.to_string()))?;
    let mut out = match jwk.common.key_algorithm {
      Some(alg) => Self::new(key, [signing_algorithm(alg)?]),
      None => Self::family(key),
    };
    out.kid = jwk.common.key_id.clone();
    Ok(out)
  }

  /// The same key with `kid`.
  #[must_use]
  pub fn with_kid(mut self, kid: impl Into<String>) -> Self {
    self.kid = Some(kid.into());
    self
  }

  /// The key id, if any.
  #[inline]
  pub fn kid(&self) -> Option<&str> {
    self.kid.as_deref()
  }

  /// The algorithms this key verifies.
  #[inline]
  pub fn algorithms(&self) -> &[Algorithm] {
    &self.algorithms
  }

  fn allows(&self, alg: Algorithm) -> bool {
    self.algorithms.contains(&alg)
  }

  /// [`AuthError::Key`] when this is an HMAC secret shorter than [`MIN_HMAC_KEY_LEN`].
  fn check_strength(&self) -> Result<(), AuthError> {
    if self.key.family() != AlgorithmFamily::Hmac {
      return Ok(());
    }
    check_hmac_len(self.key.try_get_as_bytes().map_or(0, <[u8]>::len))
  }
}

/// The JWS algorithm a JWK's `alg` names (`RSA-OAEP` and other encryption algorithms are not).
fn signing_algorithm(alg: KeyAlgorithm) -> Result<Algorithm, AuthError> {
  let name = serde_json::to_value(alg)
    .ok()
    .and_then(|v| v.as_str().map(str::to_owned))
    .unwrap_or_default();
  name
    .parse()
    .map_err(|_| AuthError::Key(format!("{name} is not a JWS signing algorithm")))
}

/// The key in `keys` for a token with `kid` and `alg`: the key with that `kid`, else a key
/// without a `kid`; for a token without `kid`, a key without one, else the only candidate.
pub(crate) fn select_key(
  keys: &[JwtKey],
  kid: Option<&str>,
  alg: Algorithm,
) -> Result<JwtKey, AuthError> {
  let candidates: Vec<&JwtKey> = keys.iter().filter(|k| k.allows(alg)).collect();
  let by_kid = kid.and_then(|kid| candidates.iter().find(|k| k.kid() == Some(kid)));
  let unnamed = || candidates.iter().find(|k| k.kid.is_none());
  let only = || (candidates.len() == 1 && kid.is_none()).then(|| &candidates[0]);
  by_kid
    .or_else(unnamed)
    .or_else(only)
    .map(|k| (*k).clone())
    .ok_or_else(|| {
      AuthError::UnknownKey(match kid {
        Some(kid) => format!("kid {kid:?} ({alg:?})"),
        None => format!("no key for {alg:?}"),
      })
    })
}

/// Which claims make up the principal of a verified token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimMapping {
  /// The principal id (default `sub`). It must be a string.
  pub subject: String,
  /// Claims whose values become roles (default `roles`).
  pub roles: Vec<String>,
  /// Claims whose values become direct permissions (default `permissions`, `scope`, `scp`).
  pub permissions: Vec<String>,
  /// The tenant claim (default `tenant`).
  pub tenant: Option<String>,
}

impl Default for ClaimMapping {
  fn default() -> Self {
    Self {
      subject: "sub".into(),
      roles: vec!["roles".into()],
      permissions: vec!["permissions".into(), "scope".into(), "scp".into()],
      tenant: Some("tenant".into()),
    }
  }
}

impl ClaimMapping {
  /// Read roles from `claims` instead (`["realm_access.roles"]` for Keycloak, `["groups"]`, …).
  #[must_use]
  pub fn roles<S: Into<String>>(mut self, claims: impl IntoIterator<Item = S>) -> Self {
    self.roles = claims.into_iter().map(Into::into).collect();
    self
  }

  /// Read permissions from `claims` instead.
  #[must_use]
  pub fn permissions<S: Into<String>>(mut self, claims: impl IntoIterator<Item = S>) -> Self {
    self.permissions = claims.into_iter().map(Into::into).collect();
    self
  }

  /// Read the principal id from `claim` instead of `sub`.
  #[must_use]
  pub fn subject(mut self, claim: impl Into<String>) -> Self {
    self.subject = claim.into();
    self
  }

  /// Read the tenant from `claim` (or nowhere, with `None`).
  #[must_use]
  pub fn tenant(mut self, claim: Option<&str>) -> Self {
    self.tenant = claim.map(str::to_owned);
    self
  }

  /// The principal of verified `claims` (every claim is kept on it).
  ///
  /// # Errors
  ///
  /// [`AuthError::InvalidClaims`] when the subject claim is missing or not a string.
  pub fn principal(&self, claims: Map<String, Value>) -> Result<AuthPrincipal, AuthError> {
    let subject = match lookup(&claims, &self.subject) {
      Some(Value::String(s)) if !s.is_empty() => s.clone(),
      _ => {
        return Err(AuthError::InvalidClaims(format!(
          "claim {} must be a non-empty string",
          self.subject
        )));
      }
    };
    let list = |names: &[String]| {
      let mut out: Vec<String> = Vec::new();
      for value in names.iter().filter_map(|n| lookup(&claims, n)) {
        for item in string_items(value) {
          if !out.contains(&item) {
            out.push(item);
          }
        }
      }
      out
    };
    let mut principal = AuthPrincipal::new(subject)
      .with_roles(list(&self.roles))
      .with_permissions(list(&self.permissions))
      .with_scheme(AuthScheme::Bearer);
    if let Some(tenant) = self
      .tenant
      .as_deref()
      .and_then(|t| lookup(&claims, t))
      .and_then(Value::as_str)
    {
      principal = principal.with_tenant(tenant);
    }
    Ok(
      claims
        .into_iter()
        .fold(principal, |p, (name, value)| p.with_claim(name, value)),
    )
  }
}

/// The claim `name`: a top-level claim of that exact name (namespaced claims contain dots),
/// else a dotted path into nested objects.
fn lookup<'c>(claims: &'c Map<String, Value>, name: &str) -> Option<&'c Value> {
  if let Some(v) = claims.get(name) {
    return Some(v);
  }
  let mut parts = name.split('.');
  let first = claims.get(parts.next()?)?;
  parts.try_fold(first, |v, part| v.get(part))
}

/// Strings in a claim value: an array's string items, or a space/comma-separated string.
fn string_items(value: &Value) -> Vec<String> {
  match value {
    Value::String(s) => split_list(s),
    Value::Array(items) => items
      .iter()
      .filter_map(Value::as_str)
      .map(str::to_owned)
      .collect(),
    _ => Vec::new(),
  }
}

/// Where a verifier's keys come from.
#[derive(Clone)]
enum Keys {
  Static(Arc<[JwtKey]>),
  #[cfg(feature = "jwks")]
  Jwks(crate::jwks::JwksClient),
}

/// Verifies bearer tokens and maps their claims to an [`AuthPrincipal`]. Cheap to clone.
///
/// See the [module documentation](self).
#[derive(Clone)]
pub struct JwtVerifier {
  keys: Keys,
  algorithms: Option<Vec<Algorithm>>,
  issuers: Vec<String>,
  audiences: Vec<String>,
  required: Vec<String>,
  require_exp: bool,
  leeway: Duration,
  mapping: ClaimMapping,
  clock: WallClock,
  any_issuer: bool,
  any_audience: bool,
}

impl fmt::Debug for JwtVerifier {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let keys = match &self.keys {
      Keys::Static(keys) => format!("{} static", keys.len()),
      #[cfg(feature = "jwks")]
      Keys::Jwks(client) => format!("JWKS {}", client.url()),
    };
    f.debug_struct("JwtVerifier")
      .field("keys", &keys)
      .field("issuers", &self.issuers)
      .field("audiences", &self.audiences)
      .field("leeway", &self.leeway)
      .finish_non_exhaustive()
  }
}

impl JwtVerifier {
  fn with_keys(keys: Keys) -> Self {
    Self {
      keys,
      algorithms: None,
      issuers: Vec::new(),
      audiences: Vec::new(),
      required: Vec::new(),
      require_exp: true,
      leeway: DEFAULT_LEEWAY,
      mapping: ClaimMapping::default(),
      clock: WallClock::system(),
      any_issuer: false,
      any_audience: false,
    }
  }

  /// A verifier over fixed `keys`.
  pub fn new(keys: impl IntoIterator<Item = JwtKey>) -> Self {
    Self::with_keys(Keys::Static(keys.into_iter().collect()))
  }

  /// A verifier over the key set `jwks` fetches (and refetches on an unknown `kid`).
  #[cfg(feature = "jwks")]
  pub fn jwks(jwks: crate::jwks::JwksClient) -> Self {
    Self::with_keys(Keys::Jwks(jwks))
  }

  /// Accept tokens whose `iss` is `issuer` (repeat for several). Once set, `iss` is required.
  #[must_use]
  pub fn issuer(mut self, issuer: impl Into<String>) -> Self {
    self.issuers.push(issuer.into());
    self
  }

  /// Accept tokens whose `aud` contains `audience` (repeat for several). Once set, `aud` is
  /// required. Set it: without an audience a token minted for another service is accepted.
  /// A verifier with neither an issuer nor an audience rejects every token (see
  /// [`dangerous_any_issuer`](Self::dangerous_any_issuer)).
  #[must_use]
  pub fn audience(mut self, audience: impl Into<String>) -> Self {
    self.audiences.push(audience.into());
    self
  }

  /// Accept tokens from any issuer even though no [`issuer`](Self::issuer) is set. Records
  /// [`DANGEROUS_ANY_ISSUER`] in the posture registry.
  ///
  /// **Dangerous.** A verifier with neither an issuer nor an audience fails every verification
  /// with [`AuthError::Config`]; calling both this and
  /// [`dangerous_any_audience`](Self::dangerous_any_audience) re-opens the 0.10.0 behaviour:
  /// any token signed by a trusted key is accepted, whoever minted it and for whichever service.
  /// Only for keys that sign tokens for this service alone (a test, a single-service secret).
  #[must_use]
  pub fn dangerous_any_issuer(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      DANGEROUS_ANY_ISSUER,
      "H7: a JwtVerifier may accept tokens from any issuer",
    );
    self.any_issuer = true;
    self
  }

  /// Accept tokens from any issuer.
  #[deprecated(
    since = "0.11.0",
    note = "renamed to dangerous_any_issuer (ADR 0013 naming)"
  )]
  #[must_use]
  pub fn any_issuer_dangerous(self) -> Self {
    self.dangerous_any_issuer()
  }

  /// Accept tokens for any audience even though no [`audience`](Self::audience) is set. Records
  /// [`DANGEROUS_ANY_AUDIENCE`] in the posture registry.
  ///
  /// **Dangerous**: see [`dangerous_any_issuer`](Self::dangerous_any_issuer); a token minted for
  /// another service of the same issuer is accepted.
  #[must_use]
  pub fn dangerous_any_audience(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      DANGEROUS_ANY_AUDIENCE,
      "H7: a JwtVerifier may accept tokens minted for any audience",
    );
    self.any_audience = true;
    self
  }

  /// Accept tokens for any audience.
  #[deprecated(
    since = "0.11.0",
    note = "renamed to dangerous_any_audience (ADR 0013 naming)"
  )]
  #[must_use]
  pub fn any_audience_dangerous(self) -> Self {
    self.dangerous_any_audience()
  }

  /// [`AuthError::Config`] unless the verifier pins an issuer or an audience, or opted out of
  /// both.
  fn check_expectations(&self) -> Result<(), AuthError> {
    let pinned = !self.issuers.is_empty() || !self.audiences.is_empty();
    if pinned || (self.any_issuer && self.any_audience) {
      return Ok(());
    }
    Err(AuthError::Config(
      "a JwtVerifier needs an issuer or an audience (or both dangerous_any_issuer() and \
       dangerous_any_audience())"
        .into(),
    ))
  }

  /// Only accept these algorithms (default: whatever the matching key's family allows).
  #[must_use]
  pub fn algorithms(mut self, algorithms: impl IntoIterator<Item = Algorithm>) -> Self {
    self.algorithms = Some(algorithms.into_iter().collect());
    self
  }

  /// Reject tokens without the claim `claim`.
  #[must_use]
  pub fn require_claim(mut self, claim: impl Into<String>) -> Self {
    self.required.push(claim.into());
    self
  }

  /// Whether `exp` must be present (default `true`). An `exp` that is present is always checked.
  #[must_use]
  pub fn require_exp(mut self, require: bool) -> Self {
    self.require_exp = require;
    self
  }

  /// Clock-skew allowance for `exp` and `nbf` (default [`DEFAULT_LEEWAY`]).
  #[must_use]
  pub fn leeway(mut self, leeway: Duration) -> Self {
    self.leeway = leeway;
    self
  }

  /// Map claims to the principal with `mapping`.
  #[must_use]
  pub fn claims(mut self, mapping: ClaimMapping) -> Self {
    self.mapping = mapping;
    self
  }

  /// Read the time from `clock` (an anchored clock follows a `TestExecutor`'s virtual time).
  #[must_use]
  pub fn with_clock(mut self, clock: WallClock) -> Self {
    self.clock = clock;
    self
  }

  /// The principal `token` stands for, or why it is rejected: [`AuthError::Malformed`] (not a
  /// JWT), [`AuthError::InvalidCredentials`] (signature, algorithm),
  /// [`AuthError::UnknownKey`], [`AuthError::Expired`], [`AuthError::NotYetValid`],
  /// [`AuthError::InvalidClaims`]; [`AuthError::Unavailable`] when the JWKS cannot be fetched;
  /// [`AuthError::Config`] when neither an issuer nor an audience is set, and
  /// [`AuthError::Key`] for an HMAC secret that is too short.
  pub fn verify(&self, token: &str) -> Effect<AuthPrincipal, AuthError> {
    let mapping = self.mapping.clone();
    self
      .verify_claims(token)
      .flat_map(move |claims| match mapping.principal(claims) {
        Ok(p) => succeed(p),
        Err(e) => fail(e),
      })
  }

  /// The verified claims of `token` (see [`verify`](Self::verify)).
  pub fn verify_claims(&self, token: &str) -> Effect<Map<String, Value>, AuthError> {
    if let Err(e) = self.check_expectations() {
      return fail(e);
    }
    let header = match decode_header(token) {
      Ok(h) => h,
      Err(_) => return fail(AuthError::Malformed("not a well-formed JWT".into())),
    };
    if let Some(allowed) = &self.algorithms
      && !allowed.contains(&header.alg)
    {
      return fail(AuthError::InvalidCredentials);
    }
    let (this, token) = (self.clone(), token.to_owned());
    let alg = header.alg;
    self.key(header.kid, alg).flat_map(move |key| {
      if let Err(e) = key.check_strength() {
        return fail(e);
      }
      let claims = match decode::<Map<String, Value>>(&token, &key.key, &this.validation(alg)) {
        Ok(data) => data.claims,
        Err(e) => return fail(AuthError::from(e)),
      };
      this
        .clock
        .now()
        .flat_map(move |now| match this.check(&claims, now) {
          Ok(()) => succeed(claims),
          Err(e) => fail(e),
        })
    })
  }

  fn key(&self, kid: Option<String>, alg: Algorithm) -> Effect<JwtKey, AuthError> {
    match &self.keys {
      Keys::Static(keys) => match select_key(keys, kid.as_deref(), alg) {
        Ok(k) => succeed(k),
        Err(e) => fail(e),
      },
      #[cfg(feature = "jwks")]
      Keys::Jwks(client) => client.key(kid, alg),
    }
  }

  fn validation(&self, alg: Algorithm) -> Validation {
    let mut v = Validation::new(alg);
    v.validate_exp = false;
    v.validate_nbf = false;
    v.leeway = 0;
    v.required_spec_claims = HashSet::new();
    if !self.issuers.is_empty() {
      v.set_issuer(&self.issuers);
      v.required_spec_claims.insert("iss".into());
    }
    v.validate_aud = !self.audiences.is_empty();
    if v.validate_aud {
      v.set_audience(&self.audiences);
      v.required_spec_claims.insert("aud".into());
    }
    v
  }

  /// Time and required-claim checks on signature-verified `claims`.
  fn check(&self, claims: &Map<String, Value>, now: UtcDateTime) -> Result<(), AuthError> {
    let now = now.0.as_second();
    let leeway = i64::try_from(self.leeway.as_secs()).unwrap_or(i64::MAX);
    let time = |name: &str| -> Result<Option<i64>, AuthError> {
      match claims.get(name) {
        None => Ok(None),
        Some(v) => v
          .as_i64()
          .or_else(|| v.as_f64().map(|f| f as i64))
          .map(Some)
          .ok_or_else(|| AuthError::InvalidClaims(format!("claim {name} must be a number"))),
      }
    };
    match time("exp")? {
      Some(exp) if now >= exp.saturating_add(leeway) => return Err(AuthError::Expired),
      None if self.require_exp => return Err(AuthError::InvalidClaims("missing claim exp".into())),
      _ => {}
    }
    if let Some(nbf) = time("nbf")?
      && now.saturating_add(leeway) < nbf
    {
      return Err(AuthError::NotYetValid);
    }
    match self.required.iter().find(|c| lookup(claims, c).is_none()) {
      Some(missing) => Err(AuthError::InvalidClaims(format!("missing claim {missing}"))),
      None => Ok(()),
    }
  }
}

/// Signs tokens: first-party access tokens and test fixtures. Cheap to clone.
///
/// [`issue`](Self::issue) writes the principal's id as `sub`, its roles, permissions and tenant
/// under the [`ClaimMapping`] default names (`roles`, `permissions`, `tenant`), its other claims
/// as they are, then `iss`, `aud`, `iat`, `exp` (now + [`ttl`](Self::ttl)) and a random `jti`.
#[derive(Clone)]
pub struct JwtIssuer {
  key: EncodingKey,
  alg: Algorithm,
  kid: Option<String>,
  issuer: Option<String>,
  audience: Option<String>,
  ttl: Duration,
  clock: WallClock,
}

impl fmt::Debug for JwtIssuer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("JwtIssuer")
      .field("alg", &self.alg)
      .field("kid", &self.kid)
      .field("issuer", &self.issuer)
      .field("audience", &self.audience)
      .field("ttl", &self.ttl)
      .finish_non_exhaustive()
  }
}

impl JwtIssuer {
  /// Sign with `key` using `alg`.
  ///
  /// # Errors
  ///
  /// [`AuthError::Key`] when `alg` does not belong to the key's family.
  pub fn new(alg: Algorithm, key: EncodingKey) -> Result<Self, AuthError> {
    if !key.family().algorithms().contains(&alg) {
      return Err(AuthError::Key(format!(
        "{alg:?} cannot sign with a {:?} key",
        key.family()
      )));
    }
    Ok(Self {
      key,
      alg,
      kid: None,
      issuer: None,
      audience: None,
      ttl: DEFAULT_TTL,
      clock: WallClock::system(),
    })
  }

  /// HS256 with `secret` (at least [`MIN_HMAC_KEY_LEN`] random bytes: with a shorter one
  /// [`issue`](Self::issue) fails with [`AuthError::Key`]).
  pub fn hs256(secret: &[u8]) -> Self {
    Self {
      key: EncodingKey::from_secret(secret),
      alg: Algorithm::HS256,
      kid: None,
      issuer: None,
      audience: None,
      ttl: DEFAULT_TTL,
      clock: WallClock::system(),
    }
  }

  /// An RSA private key in PEM (PKCS#1 or PKCS#8), signing with `alg` (RS* or PS*).
  ///
  /// # Errors
  ///
  /// [`AuthError::Key`] when `pem` is not an RSA private key or `alg` is not an RSA algorithm.
  pub fn rsa_pem(alg: Algorithm, pem: &[u8]) -> Result<Self, AuthError> {
    let key = EncodingKey::from_rsa_pem(pem).map_err(|e| AuthError::Key(e.to_string()))?;
    Self::new(alg, key)
  }

  /// An EC private key in PKCS#8 PEM, signing with `alg` (ES256 or ES384).
  ///
  /// # Errors
  ///
  /// [`AuthError::Key`] when `pem` is not an EC private key or `alg` is not an EC algorithm.
  pub fn ec_pem(alg: Algorithm, pem: &[u8]) -> Result<Self, AuthError> {
    let key = EncodingKey::from_ec_pem(pem).map_err(|e| AuthError::Key(e.to_string()))?;
    Self::new(alg, key)
  }

  /// Name the key `kid` in every token header (and in [`jwk`](Self::jwk)).
  #[must_use]
  pub fn kid(mut self, kid: impl Into<String>) -> Self {
    self.kid = Some(kid.into());
    self
  }

  /// Write `iss: issuer`.
  #[must_use]
  pub fn issuer(mut self, issuer: impl Into<String>) -> Self {
    self.issuer = Some(issuer.into());
    self
  }

  /// Write `aud: audience`.
  #[must_use]
  pub fn audience(mut self, audience: impl Into<String>) -> Self {
    self.audience = Some(audience.into());
    self
  }

  /// Tokens live `ttl` (default [`DEFAULT_TTL`]).
  #[must_use]
  pub fn ttl(mut self, ttl: Duration) -> Self {
    self.ttl = ttl;
    self
  }

  /// Read the time from `clock`.
  #[must_use]
  pub fn with_clock(mut self, clock: WallClock) -> Self {
    self.clock = clock;
    self
  }

  /// The signing algorithm.
  #[inline]
  pub fn algorithm(&self) -> Algorithm {
    self.alg
  }

  /// A signed token for `principal` (see the type documentation for the claims).
  pub fn issue(&self, principal: &AuthPrincipal) -> Effect<String, AuthError> {
    use id_effect_authz::Principal as _;
    let mut claims: Map<String, Value> = principal
      .claims()
      .iter()
      .map(|(k, v)| (k.clone(), v.clone()))
      .collect();
    claims.insert("sub".into(), principal.id().into());
    if !principal.roles().is_empty() {
      claims.insert("roles".into(), principal.roles().into());
    }
    if !principal.permissions().is_empty() {
      claims.insert("permissions".into(), principal.permissions().into());
    }
    if let Some(tenant) = principal.tenant() {
      claims.insert("tenant".into(), tenant.into());
    }
    self.issue_claims(claims)
  }

  /// A signed token carrying `claims`, plus `iss`, `aud`, `iat`, `exp` and `jti` unless
  /// `claims` already sets them.
  pub fn issue_claims(&self, claims: Map<String, Value>) -> Effect<String, AuthError> {
    if self.key.family() == AlgorithmFamily::Hmac
      && let Err(e) = check_hmac_len(self.key.as_bytes().len())
    {
      return fail(e);
    }
    let this = self.clone();
    self.clock.now().flat_map(move |now| {
      let now = now.0.as_second();
      let ttl = i64::try_from(this.ttl.as_secs()).unwrap_or(i64::MAX);
      let mut claims = claims;
      let mut default = |name: &str, value: Value| {
        claims.entry(name.to_owned()).or_insert(value);
      };
      if let Some(iss) = &this.issuer {
        default("iss", iss.as_str().into());
      }
      if let Some(aud) = &this.audience {
        default("aud", aud.as_str().into());
      }
      default("iat", now.into());
      default("exp", now.saturating_add(ttl).into());
      match crate::apikey::random_hex(16) {
        Ok(jti) => default("jti", jti.into()),
        Err(e) => return fail(e),
      }
      let mut header = Header::new(this.alg);
      header.kid.clone_from(&this.kid);
      match encode(&header, &claims, &this.key) {
        Ok(token) => succeed(token),
        Err(e) => fail(AuthError::Key(e.to_string())),
      }
    })
  }

  /// The public JSON Web Key of an asymmetric issuer (with its `kid`, `alg` and `use: sig`), to
  /// publish in a JWKS document.
  ///
  /// # Errors
  ///
  /// [`AuthError::Key`] for an HMAC issuer: its secret must never be published.
  pub fn jwk(&self) -> Result<Jwk, AuthError> {
    if self.key.family() == AlgorithmFamily::Hmac {
      return Err(AuthError::Key("an HMAC secret has no public key".into()));
    }
    let mut jwk =
      Jwk::from_encoding_key(&self.key, self.alg).map_err(|e| AuthError::Key(e.to_string()))?;
    jwk.common = CommonParameters {
      public_key_use: Some(PublicKeyUse::Signature),
      key_algorithm: Some(KeyAlgorithm::from(self.alg)),
      key_id: self.kid.clone(),
      ..CommonParameters::default()
    };
    Ok(jwk)
  }

  /// The key that verifies this issuer's tokens (the public key, or the HMAC secret).
  ///
  /// # Errors
  ///
  /// [`AuthError::Key`] when the public key cannot be derived.
  pub fn verification_key(&self) -> Result<JwtKey, AuthError> {
    let key = if self.key.family() == AlgorithmFamily::Hmac {
      JwtKey::new(DecodingKey::from_secret(self.key.as_bytes()), [self.alg])
    } else {
      JwtKey::from_jwk(&self.jwk()?)?
    };
    Ok(match &self.kid {
      Some(kid) => key.with_kid(kid.clone()),
      None => key,
    })
  }
}

#[cfg(test)]
mod tests {
  use std::time::Instant;

  use id_effect::{Cause, Exit};
  use id_effect_authz::Principal;
  use serde_json::json;

  use super::*;

  const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

  fn run<A: Send + 'static>(eff: Effect<A, AuthError>) -> Result<A, AuthError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn clock_at(secs: i64) -> WallClock {
    WallClock::anchored(Instant::now(), UtcDateTime::unsafe_make(secs * 1000))
  }

  fn claims(v: Value) -> Map<String, Value> {
    match v {
      Value::Object(m) => m,
      _ => unreachable!(),
    }
  }

  fn hs(claims_v: Value) -> String {
    let issuer = JwtIssuer::hs256(SECRET).with_clock(clock_at(1_000));
    run(issuer.issue_claims(claims(claims_v))).unwrap()
  }

  /// A verifier that pins nothing (these tests are not about `iss` / `aud`).
  fn verifier() -> JwtVerifier {
    JwtVerifier::new([JwtKey::hmac(SECRET)])
      .with_clock(clock_at(1_000))
      .dangerous_any_issuer()
      .dangerous_any_audience()
  }

  #[test]
  fn issued_tokens_round_trip_with_mapped_claims() {
    let issuer = JwtIssuer::hs256(SECRET)
      .issuer("iss-a")
      .audience("aud-a")
      .kid("k1")
      .ttl(Duration::from_secs(60))
      .with_clock(clock_at(1_000));
    assert_eq!(issuer.algorithm(), Algorithm::HS256);
    let alice = AuthPrincipal::new("alice")
      .with_role("viewer")
      .with_permission("ledger:read")
      .with_tenant("acme")
      .with_claim("email", "a@example.com");
    let token = run(issuer.issue(&alice)).unwrap();
    let v = verifier().issuer("iss-a").audience("aud-a");
    let p = run(v.verify(&token)).unwrap();
    assert_eq!(p.id(), "alice");
    assert_eq!(p.roles(), ["viewer"]);
    assert_eq!(p.permissions(), ["ledger:read"]);
    assert_eq!(Principal::tenant(&p), Some("acme"));
    assert_eq!(p.claim("email"), Some(&json!("a@example.com")));
    assert_eq!(p.claim("exp"), Some(&json!(1_060)));
    assert_eq!(p.claim("iat"), Some(&json!(1_000)));
    assert!(p.claim("jti").is_some());
    assert_eq!(p.scheme(), Some(AuthScheme::Bearer));
    let header = decode_header(&token).unwrap();
    assert_eq!(header.kid.as_deref(), Some("k1"));
    let vk = issuer.verification_key().unwrap();
    assert_eq!(vk.kid(), Some("k1"));
    assert!(
      run(
        JwtVerifier::new([vk])
          .issuer("iss-a")
          .with_clock(clock_at(1_000))
          .verify(&token)
      )
      .is_ok()
    );
    assert!(issuer.jwk().is_err(), "HMAC secrets are never published");
  }

  #[test]
  fn time_checks_use_the_clock_and_leeway() {
    let token = hs(json!({"sub": "a", "exp": 1_100, "nbf": 900}));
    let at = |secs| verifier().with_clock(clock_at(secs));
    assert!(run(at(1_000).verify(&token)).is_ok());
    assert!(run(at(1_159).verify(&token)).is_ok(), "within leeway");
    assert_eq!(run(at(1_160).verify(&token)), Err(AuthError::Expired));
    assert_eq!(
      run(at(1_100).leeway(Duration::ZERO).verify(&token)),
      Err(AuthError::Expired)
    );
    assert!(run(at(840).verify(&token)).is_ok(), "nbf within leeway");
    assert_eq!(run(at(839).verify(&token)), Err(AuthError::NotYetValid));
    let float_exp = hs(json!({"sub": "a", "exp": 1_100.5}));
    assert!(run(verifier().verify(&float_exp)).is_ok());
    let bad_exp = hs(json!({"sub": "a", "exp": "soon"}));
    assert!(matches!(
      run(verifier().verify(&bad_exp)),
      Err(AuthError::InvalidClaims(_))
    ));
  }

  #[test]
  fn exp_and_other_claims_can_be_required() {
    let no_exp = {
      let mut m = claims(json!({"sub": "a"}));
      m.insert("exp".into(), Value::Null);
      let mut header = Header::new(Algorithm::HS256);
      header.kid = None;
      m.remove("exp");
      encode(&header, &m, &EncodingKey::from_secret(SECRET)).unwrap()
    };
    assert_eq!(
      run(verifier().verify(&no_exp)),
      Err(AuthError::InvalidClaims("missing claim exp".into()))
    );
    assert!(run(verifier().require_exp(false).verify(&no_exp)).is_ok());
    let token = hs(json!({"sub": "a", "org": {"id": 7}}));
    assert!(run(verifier().require_claim("org.id").verify(&token)).is_ok());
    assert_eq!(
      run(verifier().require_claim("email").verify(&token)),
      Err(AuthError::InvalidClaims("missing claim email".into()))
    );
  }

  #[test]
  fn issuer_audience_and_signature_are_enforced() {
    let token = hs(json!({"sub": "a", "iss": "iss-a", "aud": ["aud-a", "aud-b"]}));
    assert!(run(verifier().issuer("iss-a").audience("aud-b").verify(&token)).is_ok());
    assert!(matches!(
      run(verifier().issuer("iss-x").verify(&token)),
      Err(AuthError::InvalidClaims(m)) if m.contains("iss")
    ));
    assert!(matches!(
      run(verifier().audience("aud-x").verify(&token)),
      Err(AuthError::InvalidClaims(m)) if m.contains("aud")
    ));
    let bare = hs(json!({"sub": "a"}));
    assert!(matches!(
      run(verifier().audience("aud-a").verify(&bare)),
      Err(AuthError::InvalidClaims(m)) if m.contains("aud")
    ));
    let wrong = JwtVerifier::new([JwtKey::hmac(b"another secret of at least 32 bytes")])
      .with_clock(clock_at(1_000))
      .dangerous_any_issuer()
      .dangerous_any_audience();
    assert_eq!(run(wrong.verify(&bare)), Err(AuthError::InvalidCredentials));
    // Flip the signature's first character (every base64url character there is significant).
    let at = bare.rfind('.').unwrap() + 1;
    let flipped = if bare[at..].starts_with('A') {
      "B"
    } else {
      "A"
    };
    let tampered = format!("{}{flipped}{}", &bare[..at], &bare[at + 1..]);
    assert_eq!(
      run(verifier().verify(&tampered)),
      Err(AuthError::InvalidCredentials)
    );
    assert!(matches!(
      run(verifier().verify("not.a.jwt")),
      Err(AuthError::Malformed(_))
    ));
    assert_eq!(
      run(verifier().algorithms([Algorithm::HS512]).verify(&bare)),
      Err(AuthError::InvalidCredentials)
    );
    assert!(run(verifier().algorithms([Algorithm::HS256]).verify(&bare)).is_ok());
  }

  #[test]
  fn claim_mapping_reads_nested_namespaced_and_scope_claims() {
    let token = hs(json!({
      "sub": "a",
      "uid": "u-9",
      "realm_access": {"roles": ["admin", "viewer"]},
      "https://example.com/roles": ["x"],
      "scope": "read write",
      "scp": ["read", "audit"],
      "org": "acme"
    }));
    let mapping = ClaimMapping::default()
      .roles(["realm_access.roles", "https://example.com/roles"])
      .subject("uid")
      .tenant(Some("org"));
    let p = run(verifier().claims(mapping).verify(&token)).unwrap();
    assert_eq!(p.id(), "u-9");
    assert_eq!(p.roles(), ["admin", "viewer", "x"]);
    assert_eq!(p.permissions(), ["read", "write", "audit"]);
    assert_eq!(Principal::tenant(&p), Some("acme"));
    let none = ClaimMapping::default().permissions(["nope"]).tenant(None);
    let p = run(verifier().claims(none).verify(&token)).unwrap();
    assert!(p.permissions().is_empty());
    assert_eq!(Principal::tenant(&p), None);
    let no_sub = hs(json!({"name": "a"}));
    assert!(matches!(
      run(verifier().verify(&no_sub)),
      Err(AuthError::InvalidClaims(m)) if m.contains("sub")
    ));
    assert!(string_items(&json!(7)).is_empty());
  }

  #[test]
  fn key_selection_by_kid_and_family() {
    let a = JwtKey::hmac(b"a").with_kid("a");
    let b = JwtKey::hmac(b"b").with_kid("b");
    let unnamed = JwtKey::hmac(b"u");
    let keys = [a.clone(), b.clone()];
    assert_eq!(
      select_key(&keys, Some("b"), Algorithm::HS256)
        .unwrap()
        .kid(),
      Some("b")
    );
    assert!(matches!(
      select_key(&keys, Some("c"), Algorithm::HS256),
      Err(AuthError::UnknownKey(m)) if m.contains("\"c\"")
    ));
    assert!(matches!(
      select_key(&keys, None, Algorithm::HS256),
      Err(AuthError::UnknownKey(m)) if m.contains("HS256")
    ));
    assert_eq!(
      select_key(std::slice::from_ref(&a), None, Algorithm::HS256)
        .unwrap()
        .kid(),
      Some("a")
    );
    let with_unnamed = [a, unnamed];
    assert_eq!(
      select_key(&with_unnamed, Some("zz"), Algorithm::HS256)
        .unwrap()
        .kid(),
      None
    );
    assert!(select_key(&keys, Some("a"), Algorithm::RS256).is_err());
    let narrowed = JwtKey::new(
      DecodingKey::from_secret(b"s"),
      [Algorithm::HS512, Algorithm::RS256],
    );
    assert_eq!(narrowed.algorithms(), [Algorithm::HS512]);
    assert!(format!("{narrowed:?}").contains("HS512"));
  }

  #[test]
  fn key_material_errors() {
    assert!(matches!(JwtKey::rsa_pem(b"nope"), Err(AuthError::Key(_))));
    assert!(matches!(JwtKey::ec_pem(b"nope"), Err(AuthError::Key(_))));
    assert!(matches!(JwtKey::ed_pem(b"nope"), Err(AuthError::Key(_))));
    assert!(matches!(
      JwtIssuer::rsa_pem(Algorithm::RS256, b"nope"),
      Err(AuthError::Key(_))
    ));
    assert!(matches!(
      JwtIssuer::ec_pem(Algorithm::ES256, b"nope"),
      Err(AuthError::Key(_))
    ));
    assert!(matches!(
      JwtIssuer::new(Algorithm::RS256, EncodingKey::from_secret(b"s")),
      Err(AuthError::Key(_))
    ));
    let issuer = JwtIssuer::new(Algorithm::HS384, EncodingKey::from_secret(b"s")).unwrap();
    assert!(format!("{issuer:?}").contains("HS384"));
    let verifier = verifier().issuer("i").audience("a");
    let dbg = format!("{verifier:?}");
    assert!(dbg.contains("1 static") && dbg.contains("\"i\""), "{dbg}");
  }

  #[test]
  fn h7_jwt_without_issuer_or_audience_is_rejected() {
    let token = hs(json!({"sub": "a", "iss": "iss-a", "aud": "aud-a"}));
    let lax = JwtVerifier::new([JwtKey::hmac(SECRET)]).with_clock(clock_at(1_000));
    let Err(AuthError::Config(m)) = run(lax.verify(&token)) else {
      panic!("a verifier pinning neither iss nor aud must not accept tokens")
    };
    assert!(m.contains("issuer or an audience"), "{m}");
    assert_eq!(AuthError::Config(m).http_status(), 500);
    // Pinning either is enough.
    assert!(run(lax.clone().issuer("iss-a").verify(&token)).is_ok());
    assert!(run(lax.clone().audience("aud-a").verify(&token)).is_ok());
    // A token for another audience of the same issuer is still refused once aud is pinned.
    assert!(matches!(
      run(lax.audience("aud-b").verify(&token)),
      Err(AuthError::InvalidClaims(m)) if m.contains("aud")
    ));
  }

  #[test]
  fn h7_dangerous_opt_out_is_explicit() {
    let token = hs(json!({"sub": "a"}));
    let lax = JwtVerifier::new([JwtKey::hmac(SECRET)]).with_clock(clock_at(1_000));
    // One opt-out alone does not re-open the lax default.
    for half in [
      lax.clone().dangerous_any_issuer(),
      lax.clone().dangerous_any_audience(),
    ] {
      assert!(matches!(
        run(half.verify(&token)),
        Err(AuthError::Config(_))
      ));
    }
    let both = lax.dangerous_any_issuer().dangerous_any_audience();
    assert_eq!(run(both.verify(&token)).unwrap().id(), "a");
  }

  #[test]
  fn h7_dangerous_any_issuer_and_audience_are_recorded() {
    let token = hs(json!({"sub": "a"}));
    let lax = JwtVerifier::new([JwtKey::hmac(SECRET)]).with_clock(clock_at(1_000));
    #[allow(deprecated)]
    let shim = lax.clone().any_issuer_dangerous().any_audience_dangerous();
    assert_eq!(run(shim.verify(&token)).unwrap().id(), "a");
    let _ = lax.dangerous_any_issuer().dangerous_any_audience();
    let recorded = id_effect::observability::posture::dangerous_opt_outs();
    for id in [DANGEROUS_ANY_ISSUER, DANGEROUS_ANY_AUDIENCE] {
      let (_, reopens) = recorded
        .iter()
        .find(|(found, _)| *found == id)
        .expect("recorded");
      assert!(reopens.starts_with("H7:"), "{id}: {reopens}");
    }
  }

  #[test]
  fn m15_short_hmac_key_rejected() {
    let short = b"31 bytes, one short of HS256 ok";
    assert_eq!(short.len(), MIN_HMAC_KEY_LEN - 1);
    // A short secret issues nothing ...
    let issued = run(JwtIssuer::hs256(short).issue(&AuthPrincipal::new("a")));
    assert!(
      matches!(&issued, Err(AuthError::Key(m)) if m.contains("at least 32 bytes")),
      "{issued:?}"
    );
    let other_route = JwtIssuer::new(Algorithm::HS384, EncodingKey::from_secret(short)).unwrap();
    assert!(matches!(
      run(other_route.issue(&AuthPrincipal::new("a"))),
      Err(AuthError::Key(_))
    ));
    // ... and verifies nothing, even a token its bytes did sign.
    let mut header = Header::new(Algorithm::HS256);
    header.kid = None;
    let forged = encode(
      &header,
      &claims(json!({"sub": "a", "exp": 2_000})),
      &EncodingKey::from_secret(short),
    )
    .unwrap();
    let weak = JwtVerifier::new([JwtKey::hmac(short)])
      .with_clock(clock_at(1_000))
      .dangerous_any_issuer()
      .dangerous_any_audience();
    assert!(matches!(
      run(weak.verify(&forged)),
      Err(AuthError::Key(m)) if m.contains("has 31")
    ));
    // 32 bytes are accepted.
    assert!(run(JwtIssuer::hs256(SECRET).issue(&AuthPrincipal::new("a"))).is_ok());
  }

  #[test]
  fn library_errors_map_to_auth_errors() {
    let cases = [
      (ErrorKind::InvalidSignature, AuthError::InvalidCredentials),
      (ErrorKind::ExpiredSignature, AuthError::Expired),
      (ErrorKind::ImmatureSignature, AuthError::NotYetValid),
      (
        ErrorKind::InvalidSubject,
        AuthError::InvalidClaims("sub is not accepted".into()),
      ),
      (
        ErrorKind::MissingRequiredClaim("x".into()),
        AuthError::InvalidClaims("missing claim x".into()),
      ),
      (
        ErrorKind::InvalidClaimFormat("x".into()),
        AuthError::InvalidClaims("claim x is malformed".into()),
      ),
    ];
    for (kind, expected) in cases {
      assert_eq!(AuthError::from(JwtLibError::from(kind)), expected);
    }
    assert!(matches!(
      AuthError::from(JwtLibError::from(ErrorKind::InvalidKeyFormat)),
      AuthError::Key(_)
    ));
  }
}
