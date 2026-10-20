//! Mutation idempotency (ADR 0014 D14, design note §3.2 and the §10 amendment).
//!
//! A mutation called with `Idempotency-Key: <key>` runs once; a retry with the same key and the
//! same payload gets the stored result back (header `idempotency-replayed: true`), a retry with
//! another payload is a `409` Problem, and a retry while the first call still runs is a `409`
//! Problem too. Results that a retry should run again — `InternalError`, `DeadlineExceeded`,
//! `Cancelled`, `ResourceExhausted` — are not stored.
//!
//! Keys are scoped ([`IdempotencyScope`]): an authenticated caller's keys are its own
//! (`(principal, tag, key)`); all anonymous callers share one principal, so their keys are
//! scoped by client IP as well (`(anonymous, client_ip, tag, key)`) and must be at least
//! [`MIN_ANONYMOUS_KEY_LEN`] characters ([`parse_key`]).
//!
//! [`IdempotencyStore`] is the storage seam (Postgres and Redis stores are 0.14 items);
//! [`MemoryIdempotencyStore`] is bounded: [`DEFAULT_TTL`] (24 h) and [`DEFAULT_MAX_KEYS`]
//! (100,000 keys), oldest evicted first.
//!
//! ```rust
//! use id_effect_rpc::idempotency::{
//!   Begin, IdempotencyKey, IdempotencyScope, IdempotencyStore, MemoryIdempotencyStore,
//!   StoredResult, payload_digest,
//! };
//!
//! # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
//! let store = MemoryIdempotencyStore::new();
//! let key = IdempotencyKey::new(IdempotencyScope::Principal("ada".into()), "Todo.add", "k-1");
//! let digest = payload_digest("Todo.add", br#"{"title":"milk"}"#);
//! assert_eq!(store.begin(&key, digest).await, Begin::Fresh);
//! store.complete(&key, StoredResult { digest, status: 200, body: b"{}".to_vec() }).await;
//! assert!(matches!(store.begin(&key, digest).await, Begin::Replay(_)));
//! # });
//! ```

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256};

/// The request header carrying the key.
pub const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";

/// The response header set on a replayed result.
pub const REPLAYED_HEADER: &str = "idempotency-replayed";

/// The longest key: 128 visible ASCII characters.
pub const MAX_KEY_LEN: usize = 128;

/// The shortest key an anonymous caller may use (§10 amendment).
pub const MIN_ANONYMOUS_KEY_LEN: usize = 16;

/// How long a stored result answers retries.
pub const DEFAULT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How many keys the in-memory store keeps.
pub const DEFAULT_MAX_KEYS: usize = 100_000;

/// Whose key it is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IdempotencyScope {
  /// An authenticated principal.
  Principal(String),
  /// An anonymous caller at this client IP.
  Anonymous(IpAddr),
}

/// A scoped key: `(scope, tag, key)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey {
  /// Whose key.
  pub scope: IdempotencyScope,
  /// The procedure.
  pub tag: String,
  /// The client's key.
  pub key: String,
}

impl IdempotencyKey {
  /// A scoped key.
  pub fn new(scope: IdempotencyScope, tag: impl Into<String>, key: impl Into<String>) -> Self {
    Self {
      scope,
      tag: tag.into(),
      key: key.into(),
    }
  }
}

/// A key the server refuses (`400` Problem).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
  /// Empty, longer than [`MAX_KEY_LEN`] or not visible ASCII.
  #[error("Idempotency-Key must be 1 to 128 visible ASCII characters")]
  Malformed,
  /// An anonymous caller's key shorter than [`MIN_ANONYMOUS_KEY_LEN`].
  #[error("an anonymous caller's Idempotency-Key must be at least 16 characters")]
  TooShort,
}

impl KeyError {
  /// The Problem `code` extension (`IdempotencyKeyMalformed`, `IdempotencyKeyTooShort`).
  pub fn code(self) -> &'static str {
    match self {
      Self::Malformed => "IdempotencyKeyMalformed",
      Self::TooShort => "IdempotencyKeyTooShort",
    }
  }
}

/// Validate a key header value for a caller that is `anonymous` or not.
pub fn parse_key(raw: &[u8], anonymous: bool) -> Result<String, KeyError> {
  if raw.is_empty() || raw.len() > MAX_KEY_LEN || !raw.iter().all(|b| (0x21..=0x7e).contains(b)) {
    return Err(KeyError::Malformed);
  }
  if anonymous && raw.len() < MIN_ANONYMOUS_KEY_LEN {
    return Err(KeyError::TooShort);
  }
  // Visible ASCII is UTF-8.
  Ok(String::from_utf8_lossy(raw).into_owned())
}

/// SHA-256 of the procedure tag and the payload bytes: a replay must send the same payload.
pub fn payload_digest(tag: &str, payload: &[u8]) -> [u8; 32] {
  let mut hasher = Sha256::new();
  hasher.update(tag.as_bytes());
  hasher.update([0]);
  hasher.update(payload);
  hasher.finalize().into()
}

/// A stored result: the payload digest it answers and the response it replays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredResult {
  /// [`payload_digest`] of the call that produced it.
  pub digest: [u8; 32],
  /// HTTP status of the response.
  pub status: u16,
  /// Encoded `WireResult`.
  pub body: Vec<u8>,
}

/// What [`IdempotencyStore::begin`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Begin {
  /// The key is new; it is now reserved for this call ([`IdempotencyStore::complete`] or
  /// [`IdempotencyStore::abandon`] it).
  Fresh,
  /// A result is stored for this key and payload: answer it.
  Replay(StoredResult),
  /// The key belongs to another payload (`409`).
  Mismatch,
  /// A call with this key is still running (`409`).
  InProgress,
}

/// A boxed store future.
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where idempotent results live.
pub trait IdempotencyStore: Send + Sync + 'static {
  /// Look `key` up for a call whose payload hashes to `digest`, reserving it when it is new.
  fn begin<'a>(&'a self, key: &'a IdempotencyKey, digest: [u8; 32]) -> StoreFuture<'a, Begin>;

  /// Store the reserved key's result.
  fn complete<'a>(&'a self, key: &'a IdempotencyKey, result: StoredResult) -> StoreFuture<'a, ()>;

  /// Release a reservation without a result (the call is to run again on retry).
  fn abandon<'a>(&'a self, key: &'a IdempotencyKey) -> StoreFuture<'a, ()>;
}

#[derive(Debug)]
enum Slot {
  Running([u8; 32]),
  Done(StoredResult),
}

#[derive(Debug)]
struct Entry {
  at: Instant,
  slot: Slot,
}

#[derive(Debug, Default)]
struct Memory {
  entries: HashMap<IdempotencyKey, Entry>,
  order: VecDeque<(Instant, IdempotencyKey)>,
}

/// The bounded in-memory store (D14): entries expire after the TTL; past the key limit the
/// oldest are evicted.
#[derive(Debug, Clone)]
pub struct MemoryIdempotencyStore {
  inner: Arc<Mutex<Memory>>,
  ttl: Duration,
  max_keys: usize,
}

impl Default for MemoryIdempotencyStore {
  fn default() -> Self {
    Self::new()
  }
}

impl MemoryIdempotencyStore {
  /// [`DEFAULT_TTL`], [`DEFAULT_MAX_KEYS`].
  pub fn new() -> Self {
    Self::with_limits(DEFAULT_TTL, DEFAULT_MAX_KEYS)
  }

  /// Custom limits (`max_keys` at least 1).
  pub fn with_limits(ttl: Duration, max_keys: usize) -> Self {
    Self {
      inner: Arc::new(Mutex::new(Memory::default())),
      ttl,
      max_keys: max_keys.max(1),
    }
  }

  /// Keys held now (expired ones included until the next write).
  pub fn len(&self) -> usize {
    self.lock().entries.len()
  }

  /// Whether no key is held.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Memory> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn expired(&self, at: Instant, now: Instant) -> bool {
    now.duration_since(at) >= self.ttl
  }

  /// Drop expired entries from the front, then the oldest beyond the key limit.
  fn evict(&self, memory: &mut Memory, now: Instant, room_for: usize) {
    while let Some((at, key)) = memory.order.front().cloned() {
      let stale = match memory.entries.get(&key) {
        // An order record of a key re-inserted later is stale.
        Some(entry) if entry.at != at => true,
        Some(_) => self.expired(at, now) || memory.entries.len() + room_for > self.max_keys,
        None => true,
      };
      if !stale {
        break;
      }
      memory.order.pop_front();
      if memory.entries.get(&key).is_some_and(|e| e.at == at) {
        memory.entries.remove(&key);
      }
    }
  }

  fn begin_now(&self, key: &IdempotencyKey, digest: [u8; 32]) -> Begin {
    let now = Instant::now();
    let mut memory = self.lock();
    if let Some(entry) = memory.entries.get(key)
      && !self.expired(entry.at, now)
    {
      return match &entry.slot {
        Slot::Running(d) if *d == digest => Begin::InProgress,
        Slot::Done(result) if result.digest == digest => Begin::Replay(result.clone()),
        Slot::Running(_) | Slot::Done(_) => Begin::Mismatch,
      };
    }
    memory.entries.remove(key);
    self.evict(&mut memory, now, 1);
    memory.order.push_back((now, key.clone()));
    memory.entries.insert(
      key.clone(),
      Entry {
        at: now,
        slot: Slot::Running(digest),
      },
    );
    Begin::Fresh
  }

  fn complete_now(&self, key: &IdempotencyKey, result: StoredResult) {
    let mut memory = self.lock();
    if let Some(entry) = memory.entries.get_mut(key) {
      entry.slot = Slot::Done(result);
    }
  }

  fn abandon_now(&self, key: &IdempotencyKey) {
    let mut memory = self.lock();
    if matches!(
      memory.entries.get(key),
      Some(Entry {
        slot: Slot::Running(_),
        ..
      })
    ) {
      memory.entries.remove(key);
    }
  }
}

impl IdempotencyStore for MemoryIdempotencyStore {
  fn begin<'a>(&'a self, key: &'a IdempotencyKey, digest: [u8; 32]) -> StoreFuture<'a, Begin> {
    Box::pin(std::future::ready(self.begin_now(key, digest)))
  }

  fn complete<'a>(&'a self, key: &'a IdempotencyKey, result: StoredResult) -> StoreFuture<'a, ()> {
    self.complete_now(key, result);
    Box::pin(std::future::ready(()))
  }

  fn abandon<'a>(&'a self, key: &'a IdempotencyKey) -> StoreFuture<'a, ()> {
    self.abandon_now(key);
    Box::pin(std::future::ready(()))
  }
}

/// Abandons a reservation when the call is dropped before it finished (a client disconnect).
pub(crate) struct Reservation {
  store: Arc<dyn IdempotencyStore>,
  key: Option<IdempotencyKey>,
}

impl Reservation {
  pub(crate) fn new(store: Arc<dyn IdempotencyStore>, key: IdempotencyKey) -> Self {
    Self {
      store,
      key: Some(key),
    }
  }

  /// Store `result` (or abandon when `result` is `None`) and disarm.
  pub(crate) async fn finish(mut self, result: Option<StoredResult>) {
    if let Some(key) = self.key.take() {
      match result {
        Some(result) => self.store.complete(&key, result).await,
        None => self.store.abandon(&key).await,
      }
    }
  }
}

impl Drop for Reservation {
  fn drop(&mut self) {
    if let Some(key) = self.key.take() {
      let store = self.store.clone();
      if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move { store.abandon(&key).await });
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn key(k: &str) -> IdempotencyKey {
    IdempotencyKey::new(IdempotencyScope::Principal("p".into()), "A.m", k)
  }

  fn result(digest: [u8; 32]) -> StoredResult {
    StoredResult {
      digest,
      status: 200,
      body: b"r".to_vec(),
    }
  }

  #[test]
  fn keys_are_validated() {
    assert_eq!(parse_key(b"k", false), Ok("k".into()));
    assert_eq!(parse_key(b"", false), Err(KeyError::Malformed));
    assert_eq!(parse_key(b"a b", false), Err(KeyError::Malformed));
    assert_eq!(parse_key(&[b'a'; 129], false), Err(KeyError::Malformed));
    assert_eq!(parse_key(b"short", true), Err(KeyError::TooShort));
    assert!(parse_key(b"0123456789abcdef", true).is_ok());
    assert_eq!(KeyError::TooShort.code(), "IdempotencyKeyTooShort");
    assert_eq!(KeyError::Malformed.code(), "IdempotencyKeyMalformed");
    assert!(KeyError::TooShort.to_string().contains("16"));
    assert_ne!(payload_digest("A.m", b"1"), payload_digest("A.n", b"1"));
  }

  #[tokio::test]
  async fn memory_store_reserves_replays_and_refuses_mismatches() {
    let store = MemoryIdempotencyStore::default();
    assert!(store.is_empty());
    let (d1, d2) = (payload_digest("A.m", b"1"), payload_digest("A.m", b"2"));
    assert_eq!(store.begin(&key("a"), d1).await, Begin::Fresh);
    assert_eq!(store.begin(&key("a"), d1).await, Begin::InProgress);
    assert_eq!(store.begin(&key("a"), d2).await, Begin::Mismatch);
    store.complete(&key("a"), result(d1)).await;
    assert_eq!(store.begin(&key("a"), d1).await, Begin::Replay(result(d1)));
    assert_eq!(store.begin(&key("a"), d2).await, Begin::Mismatch);
    // Abandon only releases a running reservation.
    store.abandon(&key("a")).await;
    assert_eq!(store.len(), 1);
    assert_eq!(store.begin(&key("b"), d1).await, Begin::Fresh);
    store.abandon(&key("b")).await;
    assert_eq!(store.begin(&key("b"), d1).await, Begin::Fresh);
    // Completing an unknown key stores nothing.
    store.complete(&key("zzz"), result(d1)).await;
    assert_eq!(store.len(), 2);
  }

  #[tokio::test]
  async fn memory_store_is_bounded_by_ttl_and_keys() {
    let d = payload_digest("A.m", b"1");
    let store = MemoryIdempotencyStore::with_limits(DEFAULT_TTL, 2);
    for k in ["a", "b", "c"] {
      assert_eq!(store.begin(&key(k), d).await, Begin::Fresh);
    }
    assert_eq!(store.len(), 2);
    assert_eq!(store.begin(&key("a"), d).await, Begin::Fresh, "evicted");
    let short = MemoryIdempotencyStore::with_limits(Duration::from_millis(1), 10);
    assert_eq!(short.begin(&key("a"), d).await, Begin::Fresh);
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(short.begin(&key("a"), d).await, Begin::Fresh, "expired");
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(short.begin(&key("b"), d).await, Begin::Fresh);
    assert_eq!(
      short.len(),
      1,
      "the expired entry is evicted on the next write"
    );
  }

  #[tokio::test]
  async fn reservations_abandon_when_dropped() {
    let d = payload_digest("A.m", b"1");
    let store = Arc::new(MemoryIdempotencyStore::new());
    assert_eq!(store.begin(&key("a"), d).await, Begin::Fresh);
    drop(Reservation::new(store.clone(), key("a")));
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert_eq!(store.begin(&key("a"), d).await, Begin::Fresh);
    Reservation::new(store.clone(), key("a"))
      .finish(Some(result(d)))
      .await;
    assert!(matches!(store.begin(&key("a"), d).await, Begin::Replay(_)));
    assert_eq!(store.begin(&key("c"), d).await, Begin::Fresh);
    Reservation::new(store.clone(), key("c")).finish(None).await;
    assert_eq!(store.begin(&key("c"), d).await, Begin::Fresh);
  }
}
