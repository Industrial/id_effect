//! API keys: random secrets stored as SHA-256 digests, found by a public id and compared in
//! constant time.
//!
//! A key reads `<prefix>_<id>_<secret>`: a configurable prefix (so leaked keys are easy to
//! scan for), 16 hex digits of public id that the store is searched by, and 64 hex digits
//! (256 bits) of secret. [`ApiKey::generate`] mints one and the [`ApiKeyRecord`] to store; the
//! plaintext is shown to its owner once and never stored. A 256-bit random secret needs no slow
//! password hash: its SHA-256 digest is stored, and [`ApiKeys::verify`] compares digests with
//! [`subtle`]'s constant-time equality.
//!
//! ```
//! use id_effect::Exit;
//! use id_effect_auth::{ApiKey, ApiKeys, AuthPrincipal, MemoryApiKeyStore};
//!
//! let store = MemoryApiKeyStore::new();
//! let (key, record) = ApiKey::generate("ledger", AuthPrincipal::new("svc-billing")).unwrap();
//! store.insert(record);
//!
//! let keys = ApiKeys::new(store, "ledger");
//! let Exit::Success(p) = keys.verify(key.expose()).block_on() else { panic!() };
//! assert_eq!(id_effect_authz::Principal::id(&p), "svc-billing");
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use id_effect::{Effect, fail, succeed, suspend};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::AuthError;
use crate::principal::{AuthPrincipal, AuthScheme};

/// Bytes of public id in a key (16 hex digits).
const ID_BYTES: usize = 8;
/// Bytes of secret in a key (256 bits, 64 hex digits).
const SECRET_BYTES: usize = 32;

fn hex(bytes: &[u8]) -> String {
  bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_hex(s: &str, len: usize) -> bool {
  s.len() == len
    && s
      .bytes()
      .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `len` bytes from the operating system's CSPRNG, as lowercase hex.
pub(crate) fn random_hex(len: usize) -> Result<String, AuthError> {
  let mut bytes = vec![0_u8; len];
  getrandom::fill(&mut bytes).map_err(|e| AuthError::Key(format!("random source: {e}")))?;
  Ok(hex(&bytes))
}

fn digest(secret: &str) -> [u8; 32] {
  Sha256::digest(secret.as_bytes()).into()
}

/// A plaintext API key. `Debug` shows only its prefix and id.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
  /// A fresh key `<prefix>_<id>_<secret>` for `principal`, and the record to store.
  ///
  /// # Errors
  ///
  /// [`AuthError::Key`] when `prefix` is empty or not lower-case ASCII letters and digits, or
  /// when the operating system's random source fails.
  pub fn generate(
    prefix: &str,
    principal: AuthPrincipal,
  ) -> Result<(Self, ApiKeyRecord), AuthError> {
    check_prefix(prefix)?;
    let id = random_hex(ID_BYTES)?;
    let secret = random_hex(SECRET_BYTES)?;
    let record = ApiKeyRecord {
      id: id.clone(),
      digest: digest(&secret),
      principal,
    };
    Ok((Self(format!("{prefix}_{id}_{secret}")), record))
  }

  /// The key, to hand to its owner once.
  #[inline]
  pub fn expose(&self) -> &str {
    &self.0
  }
}

impl fmt::Debug for ApiKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let shown = self.0.rsplit_once('_').map_or("", |(head, _)| head);
    write!(f, "ApiKey({shown}_…)")
  }
}

fn check_prefix(prefix: &str) -> Result<(), AuthError> {
  let ok = !prefix.is_empty()
    && prefix
      .bytes()
      .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
  if ok {
    Ok(())
  } else {
    Err(AuthError::Key(format!(
      "API key prefix {prefix:?} must be lower-case letters and digits"
    )))
  }
}

/// What a store keeps for one API key: the public id, the SHA-256 digest of the secret and the
/// principal the key authenticates as. Never the plaintext.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeyRecord {
  id: String,
  digest: [u8; 32],
  principal: AuthPrincipal,
}

impl ApiKeyRecord {
  /// A record as a store reads it back: id, the digest as 64 hex digits
  /// ([`digest_hex`](Self::digest_hex)) and the principal.
  ///
  /// # Errors
  ///
  /// [`AuthError::Malformed`] when `id` or `digest_hex` has the wrong shape.
  pub fn from_parts(
    id: impl Into<String>,
    digest_hex: &str,
    principal: AuthPrincipal,
  ) -> Result<Self, AuthError> {
    let id = id.into();
    if !is_hex(&id, ID_BYTES * 2) || !is_hex(digest_hex, 64) {
      return Err(AuthError::Malformed("API key record".into()));
    }
    let mut digest = [0_u8; 32];
    for (i, byte) in digest.iter_mut().enumerate() {
      *byte = u8::from_str_radix(&digest_hex[i * 2..i * 2 + 2], 16)
        .map_err(|_| AuthError::Malformed("API key record".into()))?;
    }
    Ok(Self {
      id,
      digest,
      principal,
    })
  }

  /// The public id (the store's lookup key).
  #[inline]
  pub fn id(&self) -> &str {
    &self.id
  }

  /// The digest of the secret, as 64 hex digits (to store).
  pub fn digest_hex(&self) -> String {
    hex(&self.digest)
  }

  /// The principal the key authenticates as.
  #[inline]
  pub fn principal(&self) -> &AuthPrincipal {
    &self.principal
  }
}

/// Where API-key records live, looked up by public id. Implement it over a table.
pub trait ApiKeyStore: Send + Sync + 'static {
  /// The record with public id `id`, if any (a revoked key has none).
  fn find(&self, id: &str) -> Effect<Option<ApiKeyRecord>, AuthError>;
}

#[derive(Default)]
struct MemoryKeysState {
  records: HashMap<String, ApiKeyRecord>,
  fail: Option<String>,
}

/// In-memory [`ApiKeyStore`] for tests and examples. Clones share the records.
#[derive(Clone, Default)]
pub struct MemoryApiKeyStore {
  inner: Arc<Mutex<MemoryKeysState>>,
}

impl fmt::Debug for MemoryApiKeyStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MemoryApiKeyStore")
      .field("len", &self.state().records.len())
      .finish()
  }
}

impl MemoryApiKeyStore {
  /// An empty store.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  fn state(&self) -> MutexGuard<'_, MemoryKeysState> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Store `record` (replacing one with the same id).
  pub fn insert(&self, record: ApiKeyRecord) {
    self.state().records.insert(record.id.clone(), record);
  }

  /// Revoke the key with public id `id`; `true` if it existed.
  pub fn revoke(&self, id: &str) -> bool {
    self.state().records.remove(id).is_some()
  }

  /// Make every later lookup fail with [`AuthError::Unavailable`]`(message)`, or recover with
  /// `None`.
  pub fn fail_with(&self, message: Option<&str>) {
    self.state().fail = message.map(str::to_owned);
  }
}

impl ApiKeyStore for MemoryApiKeyStore {
  fn find(&self, id: &str) -> Effect<Option<ApiKeyRecord>, AuthError> {
    let (this, id) = (self.clone(), id.to_owned());
    suspend(move || {
      let st = this.state();
      match &st.fail {
        Some(m) => fail(AuthError::Unavailable(m.clone())),
        None => succeed(st.records.get(&id).cloned()),
      }
    })
  }
}

/// API-key checking over an [`ApiKeyStore`] for keys with one prefix. Cheap to clone.
#[derive(Clone)]
pub struct ApiKeys {
  store: Arc<dyn ApiKeyStore>,
  prefix: Arc<str>,
}

impl fmt::Debug for ApiKeys {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ApiKeys")
      .field("prefix", &self.prefix)
      .finish_non_exhaustive()
  }
}

impl ApiKeys {
  /// Check keys `<prefix>_…` against `store`.
  pub fn new(store: impl ApiKeyStore, prefix: impl Into<String>) -> Self {
    Self {
      store: Arc::new(store),
      prefix: Arc::from(prefix.into()),
    }
  }

  /// The key prefix.
  #[inline]
  pub fn prefix(&self) -> &str {
    &self.prefix
  }

  /// The principal `presented` authenticates as (scheme [`AuthScheme::ApiKey`]).
  ///
  /// A key of the wrong shape is [`AuthError::Malformed`]; an unknown id or a wrong secret is
  /// [`AuthError::InvalidCredentials`]; a store failure is [`AuthError::Unavailable`].
  pub fn verify(&self, presented: &str) -> Effect<AuthPrincipal, AuthError> {
    let Some((id, secret)) = self.split(presented) else {
      return fail(AuthError::Malformed(
        "not an API key of this service".into(),
      ));
    };
    let presented_digest = digest(secret);
    self.store.find(id).flat_map(move |found| match found {
      Some(record) if bool::from(record.digest.ct_eq(&presented_digest)) => {
        succeed(record.principal.with_scheme(AuthScheme::ApiKey))
      }
      _ => fail(AuthError::InvalidCredentials),
    })
  }

  /// `(id, secret)` of a well-formed key with this prefix.
  fn split<'k>(&self, key: &'k str) -> Option<(&'k str, &'k str)> {
    let rest = key.strip_prefix(&*self.prefix)?.strip_prefix('_')?;
    let (id, secret) = rest.split_once('_')?;
    (is_hex(id, ID_BYTES * 2) && is_hex(secret, SECRET_BYTES * 2)).then_some((id, secret))
  }
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit};

  use super::*;

  fn run<A: Send + 'static>(eff: Effect<A, AuthError>) -> Result<A, AuthError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  #[test]
  fn generated_keys_verify_and_records_never_hold_the_secret() {
    let store = MemoryApiKeyStore::new();
    let (key, record) = ApiKey::generate("ide", AuthPrincipal::new("svc").with_role("r")).unwrap();
    assert!(key.expose().starts_with("ide_"));
    assert_eq!(key.expose().len(), 4 + 16 + 1 + 64);
    let secret = key.expose().rsplit_once('_').unwrap().1;
    assert!(!record.digest_hex().contains(secret));
    assert!(!format!("{record:?}").contains(secret));
    assert!(!format!("{key:?}").contains(secret));
    assert!(format!("{key:?}").contains(record.id()));
    store.insert(record.clone());
    let keys = ApiKeys::new(store.clone(), "ide");
    assert_eq!(keys.prefix(), "ide");
    let p = run(keys.verify(key.expose())).unwrap();
    assert_eq!(
      p,
      record.principal().clone().with_scheme(AuthScheme::ApiKey)
    );
    assert!(format!("{keys:?}").contains("ide"));
    assert!(format!("{store:?}").contains("len: 1"));
  }

  #[test]
  fn wrong_secret_unknown_id_revoked_and_malformed() {
    let store = MemoryApiKeyStore::new();
    let (key, record) = ApiKey::generate("ide", AuthPrincipal::new("svc")).unwrap();
    store.insert(record.clone());
    let keys = ApiKeys::new(store.clone(), "ide");
    let wrong = format!("ide_{}_{}", record.id(), "0".repeat(64));
    assert_eq!(run(keys.verify(&wrong)), Err(AuthError::InvalidCredentials));
    let unknown = format!("ide_{}_{}", "1".repeat(16), "0".repeat(64));
    assert_eq!(
      run(keys.verify(&unknown)),
      Err(AuthError::InvalidCredentials)
    );
    for bad in [
      "",
      "ide",
      "other_0000000000000000_00",
      "ide_zz_00",
      &format!("ide_{}", record.id()),
      &format!("ide_{}_{}", record.id(), "A".repeat(64)),
    ] {
      assert!(
        matches!(run(keys.verify(bad)), Err(AuthError::Malformed(_))),
        "{bad}"
      );
    }
    assert!(store.revoke(record.id()));
    assert!(!store.revoke(record.id()));
    assert_eq!(
      run(keys.verify(key.expose())),
      Err(AuthError::InvalidCredentials)
    );
    store.insert(record);
    store.fail_with(Some("db"));
    assert_eq!(
      run(keys.verify(key.expose())),
      Err(AuthError::Unavailable("db".into()))
    );
    store.fail_with(None);
    assert!(run(keys.verify(key.expose())).is_ok());
  }

  #[test]
  fn records_round_trip_through_hex_and_prefixes_are_checked() {
    let (_, record) = ApiKey::generate("k1", AuthPrincipal::new("svc")).unwrap();
    let back = ApiKeyRecord::from_parts(
      record.id(),
      &record.digest_hex(),
      record.principal().clone(),
    )
    .unwrap();
    assert_eq!(back, record);
    let p = || AuthPrincipal::new("x");
    assert!(ApiKeyRecord::from_parts("short", &record.digest_hex(), p()).is_err());
    assert!(ApiKeyRecord::from_parts(record.id(), "00", p()).is_err());
    for bad in ["", "Ab", "a_b", "a-b"] {
      assert!(matches!(ApiKey::generate(bad, p()), Err(AuthError::Key(_))));
    }
  }
}
