//! Session persistence trait for cookie or header-backed sessions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use id_effect::Effect;

/// Session payload stored server-side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionData {
  /// Authenticated principal id.
  pub user_id: String,
  /// Arbitrary string attributes (roles, tenant, etc.).
  pub attrs: HashMap<String, String>,
}

impl SessionData {
  /// Create a session for `user_id`.
  pub fn new(user_id: impl Into<String>) -> Self {
    Self {
      user_id: user_id.into(),
      attrs: HashMap::new(),
    }
  }
}

/// Session store failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionError {
  /// Storage I/O or lock failure.
  Storage(String),
  /// Session id not found.
  NotFound,
}

impl std::fmt::Display for SessionError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Storage(msg) => write!(f, "session storage: {msg}"),
      Self::NotFound => write!(f, "session not found"),
    }
  }
}

impl std::error::Error for SessionError {}

/// Capability: load and persist authenticated sessions.
pub trait SessionStore: Send + Sync {
  /// Load session by id.
  fn get(&self, session_id: &str) -> Effect<Option<SessionData>, SessionError, ()>;
  /// Upsert session data.
  fn put(&self, session_id: &str, data: SessionData) -> Effect<(), SessionError, ()>;
  /// Remove session.
  fn delete(&self, session_id: &str) -> Effect<(), SessionError, ()>;
}

/// In-memory session map for tests and local dev.
#[derive(Clone, Default)]
pub struct MemorySessionStore {
  inner: Arc<Mutex<HashMap<String, SessionData>>>,
}

impl SessionStore for MemorySessionStore {
  fn get(&self, session_id: &str) -> Effect<Option<SessionData>, SessionError, ()> {
    let id = session_id.to_owned();
    let inner = Arc::clone(&self.inner);
    Effect::new(move |_r| {
      let guard = inner
        .lock()
        .map_err(|e| SessionError::Storage(e.to_string()))?;
      Ok(guard.get(&id).cloned())
    })
  }

  fn put(&self, session_id: &str, data: SessionData) -> Effect<(), SessionError, ()> {
    let id = session_id.to_owned();
    let inner = Arc::clone(&self.inner);
    Effect::new(move |_r| {
      let mut guard = inner
        .lock()
        .map_err(|e| SessionError::Storage(e.to_string()))?;
      guard.insert(id, data);
      Ok(())
    })
  }

  fn delete(&self, session_id: &str) -> Effect<(), SessionError, ()> {
    let id = session_id.to_owned();
    let inner = Arc::clone(&self.inner);
    Effect::new(move |_r| {
      let mut guard = inner
        .lock()
        .map_err(|e| SessionError::Storage(e.to_string()))?;
      guard.remove(&id);
      Ok(())
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::run_blocking;

  #[test]
  fn memory_store_round_trip() {
    let store = MemorySessionStore::default();
    let sid = "sess-1";
    let data = SessionData::new("user-42");
    run_blocking(store.put(sid, data.clone()), ()).unwrap();
    let loaded = run_blocking(store.get(sid), ()).unwrap().unwrap();
    assert_eq!(loaded.user_id, "user-42");
  }

  #[test]
  fn delete_removes_session() {
    let store = MemorySessionStore::default();
    run_blocking(store.put("s", SessionData::new("u")), ()).unwrap();
    run_blocking(store.delete("s"), ()).unwrap();
    assert!(run_blocking(store.get("s"), ()).unwrap().is_none());
  }

  #[test]
  fn get_missing_returns_none() {
    let store = MemorySessionStore::default();
    assert!(run_blocking(store.get("missing"), ()).unwrap().is_none());
  }

  #[test]
  fn session_error_display() {
    assert_eq!(SessionError::NotFound.to_string(), "session not found");
    assert!(
      SessionError::Storage("io".into())
        .to_string()
        .contains("io")
    );
  }
}
