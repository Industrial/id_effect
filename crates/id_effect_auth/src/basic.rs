//! HTTP Basic credentials: a username and password checked against a [`UserLookup`] with a
//! [`PasswordHasher`].
//!
//! [`BasicAuth::authenticate`] looks the user up and verifies the password with
//! [`PasswordHasher::verify_or_dummy`], so an unknown user costs the same argon2 work as a wrong
//! password. A correct password whose stored hash used older parameters is re-hashed and handed
//! to [`UserLookup::update_password_hash`].
//!
//! ```
//! use id_effect::{Cause, Exit};
//! use id_effect_auth::{AuthError, BasicAuth, MemoryUsers, PasswordHasher, PasswordParams};
//!
//! let hasher = PasswordHasher::new(PasswordParams::new(64, 1, 1).unwrap()); // low cost for docs
//! let users = MemoryUsers::new();
//! let Exit::Success(hash) = hasher.hash("s3cret").block_on() else { panic!() };
//! users.insert("alice", hash, id_effect_auth::AuthPrincipal::new("user-1").with_role("viewer"));
//!
//! let basic = BasicAuth::new(users, hasher);
//! let Exit::Success(p) = basic.authenticate("alice", "s3cret").block_on() else { panic!() };
//! assert_eq!(id_effect_authz::Principal::id(&p), "user-1");
//! let wrong = basic.authenticate("alice", "nope").block_on();
//! assert!(matches!(wrong, Exit::Failure(Cause::Fail(AuthError::InvalidCredentials))));
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use id_effect::{Effect, fail, succeed, suspend};

use crate::error::AuthError;
use crate::password::{Password, PasswordError, PasswordHash, PasswordHasher, Verification};
use crate::principal::{AuthPrincipal, AuthScheme};

/// What a [`UserLookup`] knows about a user: the stored password hash and the principal the
/// user authenticates as.
#[derive(Clone, Debug, PartialEq)]
pub struct UserCredentials {
  /// The stored argon2id PHC hash.
  pub password_hash: PasswordHash,
  /// The principal a successful login yields (roles, tenant, …).
  pub principal: AuthPrincipal,
}

/// Capability seam: finds a user's credentials by login name. Implement it over the user table.
pub trait UserLookup: Send + Sync + 'static {
  /// The credentials of `username`, or `None` when there is no such user.
  fn find_user(&self, username: &str) -> Effect<Option<UserCredentials>, AuthError>;

  /// Store `hash`, the user's password re-hashed under the current parameters. Called after a
  /// successful login whose stored hash was outdated; the default keeps the old one.
  fn update_password_hash(&self, username: &str, hash: PasswordHash) -> Effect<(), AuthError> {
    let _ = (username, hash);
    succeed(())
  }
}

#[derive(Default)]
struct MemoryUsersState {
  users: HashMap<String, UserCredentials>,
  fail: Option<String>,
}

/// In-memory [`UserLookup`] for tests and examples. Clones share the users.
#[derive(Clone, Default)]
pub struct MemoryUsers {
  inner: Arc<Mutex<MemoryUsersState>>,
}

impl fmt::Debug for MemoryUsers {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MemoryUsers")
      .field("len", &self.state().users.len())
      .finish()
  }
}

impl MemoryUsers {
  /// No users.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  fn state(&self) -> MutexGuard<'_, MemoryUsersState> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Add or replace the user `username`.
  pub fn insert(
    &self,
    username: impl Into<String>,
    password_hash: PasswordHash,
    principal: AuthPrincipal,
  ) {
    self.state().users.insert(
      username.into(),
      UserCredentials {
        password_hash,
        principal,
      },
    );
  }

  /// The stored hash of `username` (to observe a rehash).
  pub fn password_hash(&self, username: &str) -> Option<PasswordHash> {
    self
      .state()
      .users
      .get(username)
      .map(|u| u.password_hash.clone())
  }

  /// Make every later lookup fail with [`AuthError::Unavailable`]`(message)`, or recover with
  /// `None`.
  pub fn fail_with(&self, message: Option<&str>) {
    self.state().fail = message.map(str::to_owned);
  }
}

impl UserLookup for MemoryUsers {
  fn find_user(&self, username: &str) -> Effect<Option<UserCredentials>, AuthError> {
    let (this, username) = (self.clone(), username.to_owned());
    suspend(move || {
      let st = this.state();
      match &st.fail {
        Some(m) => fail(AuthError::Unavailable(m.clone())),
        None => succeed(st.users.get(&username).cloned()),
      }
    })
  }

  fn update_password_hash(&self, username: &str, hash: PasswordHash) -> Effect<(), AuthError> {
    let (this, username) = (self.clone(), username.to_owned());
    suspend(move || {
      if let Some(user) = this.state().users.get_mut(&username) {
        user.password_hash = hash;
      }
      succeed(())
    })
  }
}

/// Basic-credential checking: a [`UserLookup`] and a [`PasswordHasher`]. Cheap to clone.
#[derive(Clone)]
pub struct BasicAuth {
  users: Arc<dyn UserLookup>,
  hasher: PasswordHasher,
}

impl fmt::Debug for BasicAuth {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("BasicAuth")
      .field("hasher", &self.hasher)
      .finish_non_exhaustive()
  }
}

fn hashing(e: PasswordError) -> AuthError {
  AuthError::Unavailable(e.to_string())
}

impl BasicAuth {
  /// Check users found by `users` with `hasher`.
  pub fn new(users: impl UserLookup, hasher: PasswordHasher) -> Self {
    Self {
      users: Arc::new(users),
      hasher,
    }
  }

  /// The principal `username` authenticates as with `password` (scheme
  /// [`AuthScheme::Basic`]). An unknown user and a wrong password are both
  /// [`AuthError::InvalidCredentials`] after the same amount of hashing work; a lookup or
  /// hashing failure is [`AuthError::Unavailable`].
  pub fn authenticate(
    &self,
    username: &str,
    password: impl Into<Password>,
  ) -> Effect<AuthPrincipal, AuthError> {
    let (this, username, password) = (self.clone(), username.to_owned(), password.into());
    self.users.find_user(&username).flat_map(move |found| {
      let stored = found.as_ref().map(|u| u.password_hash.clone());
      let users = Arc::clone(&this.users);
      this
        .hasher
        .verify_or_dummy(password, stored.as_ref())
        .map_err(hashing)
        .flat_map(move |verification| match (verification, found) {
          (Verification::Valid, Some(user)) => {
            succeed(user.principal.with_scheme(AuthScheme::Basic))
          }
          (Verification::ValidNeedsRehash(upgraded), Some(user)) => users
            .update_password_hash(&username, upgraded)
            .map(move |()| user.principal.with_scheme(AuthScheme::Basic)),
          _ => fail(AuthError::InvalidCredentials),
        })
    })
  }

  /// [`authenticate`](Self::authenticate) with the credentials of a Basic `Authorization`
  /// header's value after the scheme (see [`basic_credentials`]).
  pub fn authenticate_encoded(&self, encoded: &str) -> Effect<AuthPrincipal, AuthError> {
    match basic_credentials(encoded) {
      Ok((username, password)) => self.authenticate(&username, password),
      Err(e) => fail(e),
    }
  }
}

/// Decode the credentials of `Authorization: Basic <encoded>` (RFC 7617): base64 of
/// `username:password`, UTF-8. The username cannot contain `:`; the password can.
///
/// # Errors
///
/// [`AuthError::Malformed`] when `encoded` is not base64, not UTF-8 or has no `:`.
pub fn basic_credentials(encoded: &str) -> Result<(String, Password), AuthError> {
  let bytes = STANDARD
    .decode(encoded.trim())
    .map_err(|_| AuthError::Malformed("basic credentials are not base64".into()))?;
  let text = String::from_utf8(bytes)
    .map_err(|_| AuthError::Malformed("basic credentials are not UTF-8".into()))?;
  let (user, password) = text
    .split_once(':')
    .ok_or_else(|| AuthError::Malformed("basic credentials have no ':'".into()))?;
  Ok((user.to_owned(), Password::new(password)))
}

/// The `Authorization` value for Basic credentials (`Basic <base64(user:password)>`), for
/// clients and tests.
pub fn basic_header(username: &str, password: &str) -> String {
  format!(
    "Basic {}",
    STANDARD.encode(format!("{username}:{password}"))
  )
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit};

  use super::*;
  use crate::password::PasswordParams;

  fn run<A: Send + 'static>(eff: Effect<A, AuthError>) -> Result<A, AuthError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn hasher(m: u32) -> PasswordHasher {
    PasswordHasher::new(PasswordParams::new(m, 1, 1).unwrap())
  }

  fn hash(h: &PasswordHasher, pw: &str) -> PasswordHash {
    match h.hash(pw).block_on() {
      Exit::Success(x) => x,
      other => panic!("{other:?}"),
    }
  }

  #[test]
  fn valid_wrong_and_unknown() {
    let h = hasher(8);
    let users = MemoryUsers::new();
    users.insert(
      "alice",
      hash(&h, "pw"),
      AuthPrincipal::new("u1").with_role("r"),
    );
    let basic = BasicAuth::new(users.clone(), h);
    let p = run(basic.authenticate("alice", "pw")).unwrap();
    assert_eq!(
      p,
      AuthPrincipal::new("u1")
        .with_role("r")
        .with_scheme(AuthScheme::Basic)
    );
    assert_eq!(
      run(basic.authenticate("alice", "bad")),
      Err(AuthError::InvalidCredentials)
    );
    assert_eq!(
      run(basic.authenticate("nobody", "pw")),
      Err(AuthError::InvalidCredentials)
    );
    assert!(format!("{basic:?}").contains("BasicAuth"));
    assert!(format!("{users:?}").contains("len: 1"));
  }

  #[test]
  fn outdated_hash_is_upgraded_on_login() {
    let users = MemoryUsers::new();
    let old = hash(&hasher(8), "pw");
    users.insert("alice", old.clone(), AuthPrincipal::new("u1"));
    let basic = BasicAuth::new(users.clone(), hasher(16));
    run(basic.authenticate("alice", "pw")).unwrap();
    let new = users.password_hash("alice").unwrap();
    assert_ne!(new, old);
    assert!(new.as_str().contains("m=16"), "{new}");
    assert_eq!(users.password_hash("nobody"), None);
  }

  #[test]
  fn store_outage_and_default_rehash_hook() {
    struct NoRehash(MemoryUsers);
    impl UserLookup for NoRehash {
      fn find_user(&self, username: &str) -> Effect<Option<UserCredentials>, AuthError> {
        self.0.find_user(username)
      }
    }
    let users = MemoryUsers::new();
    users.insert("alice", hash(&hasher(8), "pw"), AuthPrincipal::new("u1"));
    let basic = BasicAuth::new(NoRehash(users.clone()), hasher(16));
    assert!(run(basic.authenticate("alice", "pw")).is_ok());
    users.fail_with(Some("db down"));
    assert_eq!(
      run(basic.authenticate("alice", "pw")),
      Err(AuthError::Unavailable("db down".into()))
    );
    users.fail_with(None);
    assert!(run(basic.authenticate("alice", "pw")).is_ok());
    assert_eq!(
      hashing(PasswordError::Blocking),
      AuthError::Unavailable(PasswordError::Blocking.to_string())
    );
  }

  #[test]
  fn header_encoding_round_trips() {
    let header = basic_header("alice", "p:w");
    let encoded = header.strip_prefix("Basic ").unwrap();
    let (user, pw) = basic_credentials(encoded).unwrap();
    assert_eq!((user.as_str(), pw.expose()), ("alice", "p:w"));
    assert!(matches!(
      basic_credentials("%%%"),
      Err(AuthError::Malformed(_))
    ));
    assert!(matches!(
      basic_credentials(&STANDARD.encode([0xff, 0xfe])),
      Err(AuthError::Malformed(_))
    ));
    assert!(matches!(
      basic_credentials(&STANDARD.encode("nocolon")),
      Err(AuthError::Malformed(_))
    ));
    let users = MemoryUsers::new();
    users.insert("alice", hash(&hasher(8), "p:w"), AuthPrincipal::new("u1"));
    let basic = BasicAuth::new(users, hasher(8));
    assert!(run(basic.authenticate_encoded(encoded)).is_ok());
    assert!(matches!(
      run(basic.authenticate_encoded("%%%")),
      Err(AuthError::Malformed(_))
    ));
  }
}
