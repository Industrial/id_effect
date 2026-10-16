//! The [`SessionStore`] trait, its capability and layer, and the in-memory store.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use id_effect::{Effect, HCons, HNil, Layer, UtcDateTime, fail, hlist, succeed, suspend};

use super::{Session, SessionError, SessionId};

/// Where session records live. Every method is an [`Effect`], so a store may do I/O.
///
/// A store does not judge expiry: [`Sessions`](super::Sessions) does, and asks the store to
/// [`purge_expired`](Self::purge_expired) records whose expiry has passed.
pub trait SessionStore: Send + Sync + 'static {
  /// Insert `session`, or replace the record with the same id.
  fn save(&self, session: Session) -> Effect<(), SessionError>;

  /// The record with id `id`, if any.
  fn load(&self, id: &SessionId) -> Effect<Option<Session>, SessionError>;

  /// Remove the record with id `id`; `true` if there was one.
  fn remove(&self, id: &SessionId) -> Effect<bool, SessionError>;

  /// Remove every record of `user_id` (logout everywhere); how many were removed.
  fn remove_user(&self, user_id: &str) -> Effect<u64, SessionError>;

  /// Remove every record whose [`expires_at`](Session::expires_at) is at or before `now`; how
  /// many were removed.
  fn purge_expired(&self, now: UtcDateTime) -> Effect<u64, SessionError>;
}

/// Capability: an `Arc<dyn SessionStore>` in the typed environment (derefs to
/// `dyn SessionStore`).
#[derive(Clone)]
pub struct SessionStoreService(pub Arc<dyn SessionStore>);

impl SessionStoreService {
  /// Wrap `store` as the capability.
  #[inline]
  pub fn new(store: impl SessionStore) -> Self {
    Self(Arc::new(store))
  }
}

impl std::ops::Deref for SessionStoreService {
  type Target = dyn SessionStore;

  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl fmt::Debug for SessionStoreService {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("SessionStoreService")
      .field(&"<dyn SessionStore>")
      .finish()
  }
}

/// The minimal typed environment holding only [`SessionStoreService`].
pub type SessionStoreEnv = HCons<SessionStoreService, HNil>;

/// A [`SessionStoreEnv`] wrapping `store`.
#[inline]
pub fn session_store_env(store: impl SessionStore) -> SessionStoreEnv {
  hlist![SessionStoreService::new(store)]
}

/// A [`Layer`] providing `store`.
#[inline]
pub fn provide_session_store<E: Send + 'static>(
  store: impl SessionStore,
) -> Layer<SessionStoreEnv, E> {
  Layer::succeed(session_store_env(store))
}

type Records = HashMap<SessionId, Session>;

#[derive(Default)]
struct MemoryState {
  records: Records,
  fail: Option<String>,
}

/// In-memory [`SessionStore`] for tests and single-process development. Clones share the
/// records. Records do not survive a restart: production uses a database-backed store.
#[derive(Clone, Default)]
pub struct MemorySessionStore {
  inner: Arc<Mutex<MemoryState>>,
}

impl fmt::Debug for MemorySessionStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MemorySessionStore")
      .field("len", &self.len())
      .finish()
  }
}

impl MemorySessionStore {
  /// An empty store.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  fn state(&self) -> MutexGuard<'_, MemoryState> {
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

  /// Make every later operation fail with [`SessionError::Storage`]`(message)` (a store outage),
  /// or recover with `None`.
  pub fn fail_with(&self, message: Option<&str>) {
    self.state().fail = message.map(str::to_owned);
  }

  /// A lazy step over the records (or the scripted failure).
  fn with<A: Send + 'static>(
    &self,
    f: impl FnOnce(&mut Records) -> A + Send + 'static,
  ) -> Effect<A, SessionError> {
    let inner = Arc::clone(&self.inner);
    suspend(move || {
      let mut st = inner.lock().unwrap_or_else(PoisonError::into_inner);
      match st.fail.clone() {
        Some(m) => fail(SessionError::Storage(m)),
        None => succeed(f(&mut st.records)),
      }
    })
  }
}

fn count(n: usize) -> u64 {
  u64::try_from(n).unwrap_or(u64::MAX)
}

impl SessionStore for MemorySessionStore {
  fn save(&self, session: Session) -> Effect<(), SessionError> {
    self.with(move |r| {
      r.insert(session.id.clone(), session);
    })
  }

  fn load(&self, id: &SessionId) -> Effect<Option<Session>, SessionError> {
    let id = id.clone();
    self.with(move |r| r.get(&id).cloned())
  }

  fn remove(&self, id: &SessionId) -> Effect<bool, SessionError> {
    let id = id.clone();
    self.with(move |r| r.remove(&id).is_some())
  }

  fn remove_user(&self, user_id: &str) -> Effect<u64, SessionError> {
    let user_id = user_id.to_owned();
    self.with(move |r| {
      let before = r.len();
      r.retain(|_, s| s.user_id != user_id);
      count(before - r.len())
    })
  }

  fn purge_expired(&self, now: UtcDateTime) -> Effect<u64, SessionError> {
    self.with(move |r| {
      let before = r.len();
      r.retain(|_, s| !s.is_expired(now));
      count(before - r.len())
    })
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use id_effect::{Cause, Exit, need};

  use super::*;

  fn run<A: Send + 'static>(eff: Effect<A, SessionError>) -> Result<A, SessionError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn at(secs: i64) -> UtcDateTime {
    UtcDateTime::unsafe_make(secs * 1000)
  }

  fn session(user: &str, expires: i64) -> Session {
    let id = SessionId::generate().unwrap();
    Session::from_parts(
      id,
      user,
      BTreeMap::new(),
      at(0),
      at(0),
      at(expires),
      at(expires),
    )
  }

  #[test]
  fn save_load_remove_round_trip() {
    let store = MemorySessionStore::new();
    assert!(store.is_empty());
    let s = session("u", 10);
    run(store.save(s.clone())).unwrap();
    assert_eq!(run(store.load(s.id())).unwrap(), Some(s.clone()));
    assert!(run(store.remove(s.id())).unwrap());
    assert!(!run(store.remove(s.id())).unwrap());
    assert_eq!(run(store.load(s.id())).unwrap(), None);
  }

  #[test]
  fn remove_user_drops_only_that_users_sessions() {
    let store = MemorySessionStore::new();
    for s in [session("a", 10), session("a", 10), session("b", 10)] {
      run(store.save(s)).unwrap();
    }
    assert_eq!(run(store.remove_user("a")).unwrap(), 2);
    assert_eq!(store.len(), 1);
    assert!(format!("{store:?}").contains("len: 1"));
  }

  #[test]
  fn purge_drops_expired_records() {
    let store = MemorySessionStore::new();
    run(store.save(session("a", 5))).unwrap();
    run(store.save(session("b", 50))).unwrap();
    assert_eq!(run(store.purge_expired(at(5))).unwrap(), 1);
    assert_eq!(store.len(), 1);
  }

  #[test]
  fn scripted_outage_fails_every_operation() {
    let store = MemorySessionStore::new();
    store.fail_with(Some("down"));
    let s = session("a", 5);
    let down = Err(SessionError::Storage("down".into()));
    assert_eq!(run(store.save(s.clone())), down);
    assert_eq!(
      run(store.load(s.id())),
      Err(SessionError::Storage("down".into()))
    );
    store.fail_with(None);
    assert_eq!(run(store.save(s)), Ok(()));
  }

  #[test]
  fn capability_and_layer() {
    let store = MemorySessionStore::new();
    let s = session("a", 5);
    let id = s.id().clone();
    run(store.save(s)).unwrap();
    let program: Effect<Option<Session>, SessionError, SessionStoreEnv> =
      need::<SessionStoreService>().flat_map(move |st| st.load(&id).narrow());
    let loaded = run(program.provide_layer(provide_session_store(store.clone()))).unwrap();
    assert_eq!(loaded.map(|s| s.user_id().to_owned()), Some("a".into()));
    assert!(format!("{:?}", session_store_env(store).0).contains("dyn SessionStore"));
  }
}
