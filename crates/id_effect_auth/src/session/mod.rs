//! Server-side sessions: opaque random ids in a cookie, session state in a [`SessionStore`].
//!
//! - [`Sessions`] is the capability handlers use: [`login`](Sessions::login) mints a fresh id
//!   (and drops the one the client presented, so a planted id never becomes authenticated),
//!   [`load`](Sessions::load) enforces the idle and absolute timeouts,
//!   [`rotate`](Sessions::rotate) re-keys a session after a privilege change,
//!   [`logout`](Sessions::logout) and [`revoke_all`](Sessions::revoke_all) end one or every
//!   session of a user.
//! - [`SessionStore`] is where records live. [`MemorySessionStore`] is the in-memory store for
//!   tests and single-process development. With feature `pg`, `PgSessionStore` keeps them in
//!   PostgreSQL (keyed by the SHA-256 of the id) over `id_effect_sql`'s `SqlClient`.
//! - [`CookieConfig`] renders the `Set-Cookie` header (`__Host-` prefix, `Secure`, `HttpOnly`,
//!   `SameSite=Lax` by default) and finds the id in a `Cookie` header.
//! - With feature `axum`, `SessionLayer` loads the request's session into
//!   `id_effect_axum::RequestCaps`.
//!
//! Times come from a [`WallClock`](id_effect::WallClock), so expiry runs in virtual time under a
//! `TestExecutor` or an anchored clock.
//!
//! ```
//! use id_effect::Exit;
//! use id_effect_auth::session::{MemorySessionStore, SessionConfig, Sessions};
//!
//! let sessions = Sessions::new(MemorySessionStore::new(), SessionConfig::default());
//! let s2 = sessions.clone();
//! let program = sessions
//!   .login("user-42", None)
//!   .flat_map(move |session| s2.load(session.id()));
//! let Exit::Success(Some(session)) = program.block_on() else { panic!("no session") };
//! assert_eq!(session.user_id(), "user-42");
//! ```

mod cookie;
#[cfg(feature = "axum")]
mod layer;
mod manager;
#[cfg(feature = "pg")]
mod pg;
mod store;

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use id_effect::UtcDateTime;

pub use cookie::{CookieConfig, SameSite};
#[cfg(feature = "axum")]
pub use layer::{SessionLayer, SessionService};
pub use manager::{Sessions, SessionsEnv, load_session, login, logout, revoke_all, sessions_layer};
#[cfg(feature = "accounts")]
pub(crate) use pg::statements;
#[cfg(feature = "pg")]
pub use pg::{PgSessionStore, pg_session_store_layer};
pub use store::{
  MemorySessionStore, SessionStore, SessionStoreEnv, SessionStoreService, provide_session_store,
  session_store_env,
};

/// Bytes of CSPRNG output in a session id (256 bits, rendered as 64 hex digits).
const ID_BYTES: usize = 32;

/// An opaque session id: 256 bits from the operating system's CSPRNG, as lowercase hex.
///
/// `Debug` shows only a prefix, so ids do not end up in logs.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(String);

impl SessionId {
  /// A fresh random id.
  ///
  /// # Errors
  ///
  /// [`SessionError::Random`] when the operating system's random source fails.
  pub fn generate() -> Result<Self, SessionError> {
    let mut bytes = [0_u8; ID_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| SessionError::Random(e.to_string()))?;
    Ok(Self(bytes.iter().map(|b| format!("{b:02x}")).collect()))
  }

  /// An id presented by a client, if it has the shape of one (64 lowercase hex digits). Anything
  /// else is rejected before it reaches the store.
  pub fn parse(s: &str) -> Option<Self> {
    let well_formed = s.len() == ID_BYTES * 2
      && s
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    well_formed.then(|| Self(s.to_owned()))
  }

  /// The id, for the cookie value or a store key.
  #[inline]
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl fmt::Debug for SessionId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "SessionId({}…)", &self.0[..self.0.len().min(6)])
  }
}

/// One server-side session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
  id: SessionId,
  user_id: String,
  attrs: BTreeMap<String, String>,
  created_at: UtcDateTime,
  last_seen_at: UtcDateTime,
  idle_expires_at: UtcDateTime,
  absolute_expires_at: UtcDateTime,
}

impl Session {
  /// A session record as a store reads it back (stores call this; handlers get sessions from
  /// [`Sessions`]).
  #[allow(clippy::too_many_arguments)]
  pub fn from_parts(
    id: SessionId,
    user_id: impl Into<String>,
    attrs: BTreeMap<String, String>,
    created_at: UtcDateTime,
    last_seen_at: UtcDateTime,
    idle_expires_at: UtcDateTime,
    absolute_expires_at: UtcDateTime,
  ) -> Self {
    Self {
      id,
      user_id: user_id.into(),
      attrs,
      created_at,
      last_seen_at,
      idle_expires_at,
      absolute_expires_at,
    }
  }

  /// The session id (the cookie value).
  #[inline]
  pub fn id(&self) -> &SessionId {
    &self.id
  }

  /// The authenticated user.
  #[inline]
  pub fn user_id(&self) -> &str {
    &self.user_id
  }

  /// String attributes (tenant, roles, CSRF secret, …).
  #[inline]
  pub fn attrs(&self) -> &BTreeMap<String, String> {
    &self.attrs
  }

  /// One attribute.
  #[inline]
  pub fn attr(&self, key: &str) -> Option<&str> {
    self.attrs.get(key).map(String::as_str)
  }

  /// Set an attribute; persist it with [`Sessions::save`].
  pub fn set_attr(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
    self.attrs.insert(key.into(), value.into());
    self
  }

  /// Remove an attribute; persist it with [`Sessions::save`].
  pub fn remove_attr(&mut self, key: &str) -> Option<String> {
    self.attrs.remove(key)
  }

  /// When the session was created (at login).
  #[inline]
  pub fn created_at(&self) -> UtcDateTime {
    self.created_at
  }

  /// When the session was last used (to within [`SessionConfig::touch_interval`]).
  #[inline]
  pub fn last_seen_at(&self) -> UtcDateTime {
    self.last_seen_at
  }

  /// When the session ends if it is not used again (idle timeout).
  #[inline]
  pub fn idle_expires_at(&self) -> UtcDateTime {
    self.idle_expires_at
  }

  /// When the session ends however much it is used (absolute timeout).
  #[inline]
  pub fn absolute_expires_at(&self) -> UtcDateTime {
    self.absolute_expires_at
  }

  /// The earlier of the two expiries.
  #[inline]
  pub fn expires_at(&self) -> UtcDateTime {
    self.idle_expires_at.min(self.absolute_expires_at)
  }

  /// `true` once `now` reached either expiry.
  #[inline]
  pub fn is_expired(&self, now: UtcDateTime) -> bool {
    now >= self.expires_at()
  }
}

/// Session lifetimes and the cookie that carries the id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConfig {
  /// A session unused for this long ends (default 30 minutes).
  pub idle_timeout: Duration,
  /// A session ends this long after login, however much it is used (default 8 hours, the upper
  /// end of OWASP's session management guidance for absolute timeouts).
  pub absolute_timeout: Duration,
  /// [`Sessions::load`] writes the new last-seen time back only when the stored one is at least
  /// this old, so reads do not each cost a write (default 1 minute).
  pub touch_interval: Duration,
  /// The cookie (default [`CookieConfig::default`]).
  pub cookie: CookieConfig,
}

impl Default for SessionConfig {
  fn default() -> Self {
    Self {
      idle_timeout: Duration::from_secs(30 * 60),
      absolute_timeout: Duration::from_secs(8 * 60 * 60),
      touch_interval: Duration::from_secs(60),
      cookie: CookieConfig::default(),
    }
  }
}

/// Session failures. A missing or expired session is not an error: it is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionError {
  /// The store failed (I/O, lock, query).
  Storage(String),
  /// The operating system's random source failed while minting an id.
  Random(String),
}

impl fmt::Display for SessionError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Storage(m) => write!(f, "session storage: {m}"),
      Self::Random(m) => write!(f, "session id generation: {m}"),
    }
  }
}

impl std::error::Error for SessionError {}

#[cfg(test)]
mod tests {
  use super::*;

  fn at(secs: i64) -> UtcDateTime {
    UtcDateTime::unsafe_make(secs * 1000)
  }

  #[test]
  fn generated_ids_are_random_hex_and_parse_back() {
    let a = SessionId::generate().unwrap();
    let b = SessionId::generate().unwrap();
    assert_ne!(a, b);
    assert_eq!(a.as_str().len(), 64);
    assert_eq!(SessionId::parse(a.as_str()), Some(a.clone()));
    let shown = format!("{a:?}");
    assert_eq!(shown, format!("SessionId({}…)", &a.as_str()[..6]));
  }

  #[test]
  fn malformed_ids_are_rejected() {
    assert_eq!(SessionId::parse(""), None);
    assert_eq!(SessionId::parse(&"A".repeat(64)), None, "uppercase");
    assert_eq!(SessionId::parse(&"g".repeat(64)), None, "not hex");
    assert_eq!(SessionId::parse(&"a".repeat(63)), None, "short");
    assert!(SessionId::parse(&"0f".repeat(32)).is_some());
  }

  #[test]
  fn expiry_is_the_earlier_of_idle_and_absolute() {
    let id = SessionId::generate().unwrap();
    let mut s = Session::from_parts(id, "u", BTreeMap::new(), at(0), at(0), at(10), at(100));
    assert_eq!(s.expires_at(), at(10));
    assert!(!s.is_expired(at(9)));
    assert!(s.is_expired(at(10)));
    s.idle_expires_at = at(200);
    assert_eq!(s.expires_at(), at(100));
    assert_eq!(
      (
        s.created_at(),
        s.last_seen_at(),
        s.idle_expires_at(),
        s.absolute_expires_at()
      ),
      (at(0), at(0), at(200), at(100))
    );
  }

  #[test]
  fn attrs_can_be_set_read_and_removed() {
    let id = SessionId::generate().unwrap();
    let mut s = Session::from_parts(id, "u", BTreeMap::new(), at(0), at(0), at(1), at(1));
    s.set_attr("tenant", "acme").set_attr("role", "admin");
    assert_eq!(s.attr("tenant"), Some("acme"));
    assert_eq!(s.attrs().len(), 2);
    assert_eq!(s.remove_attr("role"), Some("admin".to_owned()));
    assert_eq!(s.attr("role"), None);
    assert_eq!(s.user_id(), "u");
  }

  #[test]
  fn defaults_and_errors() {
    let c = SessionConfig::default();
    assert_eq!(c.idle_timeout, Duration::from_secs(1800));
    assert_eq!(c.absolute_timeout, Duration::from_secs(28_800));
    assert!(
      SessionError::Storage("io".into())
        .to_string()
        .contains("storage: io")
    );
    assert!(
      SessionError::Random("x".into())
        .to_string()
        .contains("generation")
    );
  }
}
