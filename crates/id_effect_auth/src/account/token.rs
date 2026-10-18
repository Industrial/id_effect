//! Account tokens: what an email verification or password reset link carries, the
//! [`TokenRecord`] a store keeps for it, the [`AccountTokenStore`] trait and the in-memory store.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use id_effect::{Effect, UtcDateTime, fail, succeed, suspend};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::AccountError;
use crate::apikey::random_hex;

/// Bytes of public id in a token (16 hex digits).
const ID_BYTES: usize = 8;
/// Bytes of secret in a token (256 bits, 64 hex digits).
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

fn digest(secret: &str) -> [u8; 32] {
  Sha256::digest(secret.as_bytes()).into()
}

/// What a token proves when it is redeemed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
  /// The account's owner reads mail at its address (the verification link).
  VerifyEmail,
  /// The account's owner may choose a new password (the reset link).
  ResetPassword,
}

impl TokenKind {
  /// `verify_email` or `reset_password` (the stored `kind`).
  pub fn as_str(self) -> &'static str {
    match self {
      Self::VerifyEmail => "verify_email",
      Self::ResetPassword => "reset_password",
    }
  }

  /// The kind stored as `s`.
  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "verify_email" => Some(Self::VerifyEmail),
      "reset_password" => Some(Self::ResetPassword),
      _ => None,
    }
  }
}

impl fmt::Display for TokenKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// A plaintext account token, `<id>_<secret>`: 16 hex digits of public id and 64 hex digits
/// (256 bits) of secret from the operating system's CSPRNG. It goes into the mail's link and
/// nowhere else; `Debug` shows only the id.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountToken(String);

impl AccountToken {
  /// A fresh token of `kind` for `account_id`, valid until `expires_at`, and the record to store
  /// (the id, the SHA-256 of the secret, never the secret).
  ///
  /// # Errors
  ///
  /// [`AccountError::Random`] when the operating system's random source fails.
  pub fn generate(
    kind: TokenKind,
    account_id: impl Into<String>,
    expires_at: UtcDateTime,
  ) -> Result<(Self, TokenRecord), AccountError> {
    let random = |len| random_hex(len).map_err(|e| AccountError::Random(e.to_string()));
    let id = random(ID_BYTES)?;
    let secret = random(SECRET_BYTES)?;
    let record = TokenRecord {
      id: id.clone(),
      kind,
      account_id: account_id.into(),
      digest: digest(&secret),
      expires_at,
    };
    Ok((Self(format!("{id}_{secret}")), record))
  }

  /// The plaintext, for the link in the mail.
  #[inline]
  pub fn expose(&self) -> &str {
    &self.0
  }

  /// `(id, secret)` of a presented token, if it has the shape of one. Anything else is rejected
  /// before it reaches the store.
  pub(crate) fn split(presented: &str) -> Option<(&str, &str)> {
    let (id, secret) = presented.split_once('_')?;
    (is_hex(id, ID_BYTES * 2) && is_hex(secret, SECRET_BYTES * 2)).then_some((id, secret))
  }
}

impl fmt::Debug for AccountToken {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let id = self.0.split_once('_').map_or("", |(id, _)| id);
    write!(f, "AccountToken({id}_…)")
  }
}

/// What a store keeps for one token: its public id, kind, account, the SHA-256 digest of the
/// secret and the expiry. Never the plaintext.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenRecord {
  id: String,
  kind: TokenKind,
  account_id: String,
  digest: [u8; 32],
  expires_at: UtcDateTime,
}

impl TokenRecord {
  /// A record as a store reads it back: id, kind, account, the digest as 64 hex digits
  /// ([`digest_hex`](Self::digest_hex)) and the expiry.
  ///
  /// # Errors
  ///
  /// [`AccountError::Storage`] when `id` or `digest_hex` has the wrong shape.
  pub fn from_parts(
    id: impl Into<String>,
    kind: TokenKind,
    account_id: impl Into<String>,
    digest_hex: &str,
    expires_at: UtcDateTime,
  ) -> Result<Self, AccountError> {
    let id = id.into();
    let malformed = || AccountError::Storage("malformed account token record".into());
    if !is_hex(&id, ID_BYTES * 2) || !is_hex(digest_hex, 64) {
      return Err(malformed());
    }
    let mut digest = [0_u8; 32];
    for (i, byte) in digest.iter_mut().enumerate() {
      *byte = u8::from_str_radix(&digest_hex[i * 2..i * 2 + 2], 16).map_err(|_| malformed())?;
    }
    Ok(Self {
      id,
      kind,
      account_id: account_id.into(),
      digest,
      expires_at,
    })
  }

  /// The public id (the store's lookup key).
  #[inline]
  pub fn id(&self) -> &str {
    &self.id
  }

  /// What the token proves.
  #[inline]
  pub fn kind(&self) -> TokenKind {
    self.kind
  }

  /// The account it was issued for.
  #[inline]
  pub fn account_id(&self) -> &str {
    &self.account_id
  }

  /// The digest of the secret, as 64 hex digits (to store).
  pub fn digest_hex(&self) -> String {
    hex(&self.digest)
  }

  /// When the token stops being accepted.
  #[inline]
  pub fn expires_at(&self) -> UtcDateTime {
    self.expires_at
  }

  /// `true` when `secret` is this token's secret: SHA-256 digests compared in constant time.
  pub(crate) fn matches(&self, secret: &str) -> bool {
    bool::from(digest(secret).ct_eq(&self.digest))
  }
}

/// Where account tokens live, found by public id. Every method is an [`Effect`], so a store may
/// do I/O.
///
/// A store does not judge tokens: [`Accounts`](super::Accounts) checks the secret, kind and
/// expiry, and redeems a token only when [`consume`](Self::consume) removed it, so of two
/// requests presenting the same token at once exactly one succeeds.
pub trait AccountTokenStore: Send + Sync + 'static {
  /// Store `record` (its id is fresh).
  fn insert(&self, record: TokenRecord) -> Effect<(), AccountError>;

  /// The record with public id `id`, if any.
  fn find(&self, id: &str) -> Effect<Option<TokenRecord>, AccountError>;

  /// Remove the record with public id `id`; `true` only for the caller that removed it.
  fn consume(&self, id: &str) -> Effect<bool, AccountError>;

  /// Remove every record of `kind` for `account_id`; how many were removed.
  fn remove_account(&self, account_id: &str, kind: TokenKind) -> Effect<u64, AccountError>;

  /// Remove every record whose expiry is at or before `now`; how many were removed.
  fn purge_expired(&self, now: UtcDateTime) -> Effect<u64, AccountError>;
}

#[derive(Default)]
struct MemoryTokensState {
  records: HashMap<String, TokenRecord>,
  fail: Option<String>,
}

/// In-memory [`AccountTokenStore`] for tests and single-process development. Clones share the
/// records.
#[derive(Clone, Default)]
pub struct MemoryAccountTokenStore {
  inner: Arc<Mutex<MemoryTokensState>>,
}

impl fmt::Debug for MemoryAccountTokenStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MemoryAccountTokenStore")
      .field("len", &self.len())
      .finish()
  }
}

impl MemoryAccountTokenStore {
  /// An empty store.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  fn state(&self) -> MutexGuard<'_, MemoryTokensState> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Number of records, expired or not.
  pub fn len(&self) -> usize {
    self.state().records.len()
  }

  /// `true` when the store holds no record.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Every record (in no particular order), as a database reader would see them.
  pub fn records(&self) -> Vec<TokenRecord> {
    self.state().records.values().cloned().collect()
  }

  /// Make every later operation fail with [`AccountError::Storage`]`(message)`, or recover with
  /// `None`.
  pub fn fail_with(&self, message: Option<&str>) {
    self.state().fail = message.map(str::to_owned);
  }

  /// A lazy step over the records (or the scripted failure).
  fn with<A: Send + 'static>(
    &self,
    f: impl FnOnce(&mut HashMap<String, TokenRecord>) -> A + Send + 'static,
  ) -> Effect<A, AccountError> {
    let this = self.clone();
    suspend(move || {
      let mut st = this.state();
      match st.fail.clone() {
        Some(m) => fail(AccountError::Storage(m)),
        None => succeed(f(&mut st.records)),
      }
    })
  }
}

fn count(n: usize) -> u64 {
  u64::try_from(n).unwrap_or(u64::MAX)
}

impl AccountTokenStore for MemoryAccountTokenStore {
  fn insert(&self, record: TokenRecord) -> Effect<(), AccountError> {
    self.with(move |records| {
      records.insert(record.id.clone(), record);
    })
  }

  fn find(&self, id: &str) -> Effect<Option<TokenRecord>, AccountError> {
    let id = id.to_owned();
    self.with(move |records| records.get(&id).cloned())
  }

  fn consume(&self, id: &str) -> Effect<bool, AccountError> {
    let id = id.to_owned();
    self.with(move |records| records.remove(&id).is_some())
  }

  fn remove_account(&self, account_id: &str, kind: TokenKind) -> Effect<u64, AccountError> {
    let account_id = account_id.to_owned();
    self.with(move |records| {
      let before = records.len();
      records.retain(|_, r| !(r.account_id == account_id && r.kind == kind));
      count(before - records.len())
    })
  }

  fn purge_expired(&self, now: UtcDateTime) -> Effect<u64, AccountError> {
    self.with(move |records| {
      let before = records.len();
      records.retain(|_, r| r.expires_at > now);
      count(before - records.len())
    })
  }
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit};

  use super::*;

  fn at(secs: i64) -> UtcDateTime {
    UtcDateTime::unsafe_make(secs * 1000)
  }

  fn run<A: Send + 'static>(eff: Effect<A, AccountError>) -> Result<A, AccountError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  #[test]
  fn generated_tokens_split_and_match_only_their_record() {
    let (token, record) = AccountToken::generate(TokenKind::ResetPassword, "a1", at(9)).unwrap();
    let (id, secret) = AccountToken::split(token.expose()).expect("well formed");
    assert_eq!(id, record.id());
    assert!(record.matches(secret));
    assert!(!record.matches(&"0".repeat(64)));
    assert_eq!(record.kind(), TokenKind::ResetPassword);
    assert_eq!(record.account_id(), "a1");
    assert_eq!(record.expires_at(), at(9));
    assert!(!record.digest_hex().contains(secret));
    assert_eq!(format!("{token:?}"), format!("AccountToken({id}_…)"));
    let (other, _) = AccountToken::generate(TokenKind::ResetPassword, "a1", at(9)).unwrap();
    assert_ne!(token, other);
  }

  #[test]
  fn malformed_tokens_and_records_are_rejected() {
    for bad in [
      "",
      "abc",
      &format!("{}_{}", "0".repeat(16), "0".repeat(63)),
      &format!("{}_{}", "G".repeat(16), "0".repeat(64)),
      &format!("{}{}", "0".repeat(16), "0".repeat(64)),
    ] {
      assert_eq!(AccountToken::split(bad), None, "{bad}");
    }
    let (_, record) = AccountToken::generate(TokenKind::VerifyEmail, "a", at(1)).unwrap();
    let back =
      TokenRecord::from_parts(record.id(), record.kind(), "a", &record.digest_hex(), at(1))
        .unwrap();
    assert_eq!(back, record);
    assert!(TokenRecord::from_parts("x", TokenKind::VerifyEmail, "a", "00", at(1)).is_err());
  }

  #[test]
  fn kinds_round_trip_their_names() {
    for kind in [TokenKind::VerifyEmail, TokenKind::ResetPassword] {
      assert_eq!(TokenKind::parse(kind.as_str()), Some(kind));
      assert_eq!(kind.to_string(), kind.as_str());
    }
    assert_eq!(TokenKind::parse("other"), None);
  }

  #[test]
  fn the_memory_store_consumes_once_and_purges() {
    let store = MemoryAccountTokenStore::new();
    let (_, a) = AccountToken::generate(TokenKind::VerifyEmail, "a", at(10)).unwrap();
    let (_, b) = AccountToken::generate(TokenKind::ResetPassword, "a", at(10)).unwrap();
    let (_, c) = AccountToken::generate(TokenKind::ResetPassword, "b", at(5)).unwrap();
    for r in [&a, &b, &c] {
      run(store.insert(r.clone())).unwrap();
    }
    assert_eq!(run(store.find(a.id())), Ok(Some(a.clone())));
    assert_eq!(run(store.consume(a.id())), Ok(true));
    assert_eq!(run(store.consume(a.id())), Ok(false));
    assert_eq!(
      run(store.remove_account("a", TokenKind::ResetPassword)),
      Ok(1)
    );
    assert_eq!(run(store.purge_expired(at(5))), Ok(1));
    assert!(store.is_empty());
    store.fail_with(Some("down"));
    assert!(matches!(run(store.find("x")), Err(AccountError::Storage(m)) if m == "down"));
    assert!(format!("{store:?}").contains("len"));
  }
}
