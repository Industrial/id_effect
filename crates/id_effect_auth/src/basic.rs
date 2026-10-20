//! HTTP Basic credentials: a username and password checked against a [`UserLookup`] with a
//! [`PasswordHasher`].
//!
//! [`BasicAuth::authenticate`] looks the user up and verifies the password with
//! [`PasswordHasher::verify_or_dummy`], so an unknown user costs the same argon2 work as a wrong
//! password. A correct password whose stored hash used older parameters is re-hashed and handed
//! to [`UserLookup::update_password_hash`].
//!
//! Before any of that, a password longer than [`MAX_PASSWORD_LEN`] bytes is rejected, and a
//! [`LoginThrottle`] (on by default) refuses attempts for an account or client address with too
//! many recent failures with [`AuthError::TooManyAttempts`], so guessing costs time and the
//! server's argon2 work stays bounded.
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
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use id_effect::{Effect, fail, succeed, suspend};

use crate::error::AuthError;
use crate::password::{
  MAX_PASSWORD_LEN, Password, PasswordError, PasswordHash, PasswordHasher, Verification,
};
use crate::principal::{AuthPrincipal, AuthScheme};
use crate::throttle::LoginThrottle;

/// Posture id of [`BasicAuth::dangerous_without_throttle`] (and the deprecated
/// [`BasicAuth::without_throttle_dangerous`]).
pub const DANGEROUS_WITHOUT_THROTTLE: &str =
  "id_effect_auth::basic::BasicAuth::dangerous_without_throttle";

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

/// Basic-credential checking: a [`UserLookup`], a [`PasswordHasher`] and a [`LoginThrottle`].
/// Cheap to clone; clones share the throttle's counts.
#[derive(Clone)]
pub struct BasicAuth {
  users: Arc<dyn UserLookup>,
  hasher: PasswordHasher,
  throttle: Option<LoginThrottle>,
}

impl fmt::Debug for BasicAuth {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("BasicAuth")
      .field("hasher", &self.hasher)
      .field("throttle", &self.throttle)
      .finish_non_exhaustive()
  }
}

fn hashing(e: PasswordError) -> AuthError {
  AuthError::Unavailable(e.to_string())
}

impl BasicAuth {
  /// Check users found by `users` with `hasher`, throttled by a default [`LoginThrottle`] (in
  /// memory, per process: it records
  /// [`MEMORY_THROTTLE_STORE_POSTURE`](crate::MEMORY_THROTTLE_STORE_POSTURE), even when
  /// [`with_throttle`](Self::with_throttle) replaces it; see [`throttled`](Self::throttled)).
  pub fn new(users: impl UserLookup, hasher: PasswordHasher) -> Self {
    Self::throttled(users, hasher, LoginThrottle::default())
  }

  /// Check users found by `users` with `hasher`, throttled by `throttle` (a shared store, for
  /// example): no default in-memory store is created, so a `throttle` over a shared store
  /// records nothing in the posture registry.
  pub fn throttled(
    users: impl UserLookup,
    hasher: PasswordHasher,
    throttle: LoginThrottle,
  ) -> Self {
    Self {
      users: Arc::new(users),
      hasher,
      throttle: Some(throttle),
    }
  }

  /// The same check throttled by `throttle` instead (other delays, a shared store).
  #[must_use]
  pub fn with_throttle(mut self, throttle: LoginThrottle) -> Self {
    self.throttle = Some(throttle);
    self
  }

  /// The same check without failed-login throttling. Records [`DANGEROUS_WITHOUT_THROTTLE`] in
  /// the posture registry.
  ///
  /// **Dangerous**: it re-opens unlimited online password guessing against every account, and
  /// lets a client keep the server busy hashing. Only for a deployment that throttles failed
  /// logins in front of this check.
  #[must_use]
  pub fn dangerous_without_throttle(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      DANGEROUS_WITHOUT_THROTTLE,
      "H5: Basic logins are not throttled, so online password guessing is unlimited",
    );
    self.throttle = None;
    self
  }

  /// The same check without failed-login throttling.
  #[deprecated(
    since = "0.11.0",
    note = "renamed to dangerous_without_throttle (ADR 0013 naming)"
  )]
  #[must_use]
  pub fn without_throttle_dangerous(self) -> Self {
    self.dangerous_without_throttle()
  }

  /// The principal `username` authenticates as with `password` (scheme
  /// [`AuthScheme::Basic`]). An unknown user and a wrong password are both
  /// [`AuthError::InvalidCredentials`] after the same amount of hashing work; a lookup or
  /// hashing failure is [`AuthError::Unavailable`]. The throttle counts the attempt against the
  /// account only; use [`authenticate_from`](Self::authenticate_from) when the client address
  /// is known.
  pub fn authenticate(
    &self,
    username: &str,
    password: impl Into<Password>,
  ) -> Effect<AuthPrincipal, AuthError> {
    self.authenticate_from(username, password, None)
  }

  /// [`authenticate`](Self::authenticate) for a request from `client`, which the throttle also
  /// counts failures against. A password longer than [`MAX_PASSWORD_LEN`] bytes is
  /// [`AuthError::Malformed`], and a throttled attempt [`AuthError::TooManyAttempts`], both
  /// before the user is looked up or anything is hashed.
  pub fn authenticate_from(
    &self,
    username: &str,
    password: impl Into<Password>,
    client: Option<IpAddr>,
  ) -> Effect<AuthPrincipal, AuthError> {
    let password = password.into();
    if password.expose().len() > MAX_PASSWORD_LEN {
      return fail(AuthError::Malformed(format!(
        "the password is longer than {MAX_PASSWORD_LEN} bytes"
      )));
    }
    let (this, username) = (self.clone(), username.to_owned());
    let allowed = match &self.throttle {
      Some(t) => t.check(&username, client),
      None => succeed(()),
    };
    allowed.flat_map(move |()| {
      let users = Arc::clone(&this.users);
      users.find_user(&username).flat_map(move |found| {
        let stored = found.as_ref().map(|u| u.password_hash.clone());
        let (users, throttle) = (Arc::clone(&this.users), this.throttle.clone());
        this
          .hasher
          .verify_or_dummy(password, stored.as_ref())
          .map_err(hashing)
          .flat_map(move |verification| {
            let principal = match (verification, found) {
              (Verification::Valid, Some(user)) => {
                succeed(user.principal.with_scheme(AuthScheme::Basic))
              }
              (Verification::ValidNeedsRehash(upgraded), Some(user)) => users
                .update_password_hash(&username, upgraded)
                .map(move |()| user.principal.with_scheme(AuthScheme::Basic)),
              _ => {
                return match throttle {
                  Some(t) => t
                    .record_failure(&username, client)
                    .flat_map(|()| fail(AuthError::InvalidCredentials)),
                  None => fail(AuthError::InvalidCredentials),
                };
              }
            };
            match throttle {
              Some(t) => principal.flat_map(move |p| t.record_success(&username).map(move |()| p)),
              None => principal,
            }
          })
      })
    })
  }

  /// [`authenticate`](Self::authenticate) with the credentials of a Basic `Authorization`
  /// header's value after the scheme (see [`basic_credentials`]).
  pub fn authenticate_encoded(&self, encoded: &str) -> Effect<AuthPrincipal, AuthError> {
    self.authenticate_encoded_from(encoded, None)
  }

  /// [`authenticate_from`](Self::authenticate_from) with the credentials of a Basic
  /// `Authorization` header's value after the scheme.
  pub fn authenticate_encoded_from(
    &self,
    encoded: &str,
    client: Option<IpAddr>,
  ) -> Effect<AuthPrincipal, AuthError> {
    match basic_credentials(encoded) {
      Ok((username, password)) => self.authenticate_from(&username, password, client),
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

  /// A [`UserLookup`] that counts its lookups (every lookup is followed by hashing).
  #[derive(Clone, Default)]
  struct Counting {
    users: MemoryUsers,
    lookups: Arc<std::sync::atomic::AtomicUsize>,
  }

  impl Counting {
    fn lookups(&self) -> usize {
      self.lookups.load(std::sync::atomic::Ordering::SeqCst)
    }
  }

  impl UserLookup for Counting {
    fn find_user(&self, username: &str) -> Effect<Option<UserCredentials>, AuthError> {
      self
        .lookups
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      self.users.find_user(username)
    }
  }

  #[test]
  fn h5_throttle_checked_before_hashing() {
    let h = hasher(8);
    let users = Counting::default();
    users
      .users
      .insert("alice", hash(&h, "pw"), AuthPrincipal::new("u1"));
    let basic = BasicAuth::new(users.clone(), h);
    for i in 1..=5 {
      assert_eq!(
        run(basic.authenticate("alice", "guess")),
        Err(AuthError::InvalidCredentials)
      );
      assert_eq!(users.lookups(), i);
    }
    // Locked: refused before the lookup and the argon2 work behind it, even with the right
    // password, and the lock does not grow the hashing count.
    for pw in ["guess", "pw"] {
      assert!(matches!(
        run(basic.authenticate("alice", pw)),
        Err(AuthError::TooManyAttempts { .. })
      ));
    }
    assert_eq!(users.lookups(), 5, "no lookup, so no hashing, while locked");
    // Another account is not locked by alice's failures (no client address here).
    assert_eq!(
      run(basic.authenticate("bob", "x")),
      Err(AuthError::InvalidCredentials)
    );
    // The opt-out hashes every attempt.
    let open = basic.clone().dangerous_without_throttle();
    assert!(run(open.authenticate("alice", "pw")).is_ok());
    assert!(format!("{open:?}").contains("throttle: None"));
  }

  #[test]
  fn h5_dangerous_without_throttle_is_recorded() {
    let basic = BasicAuth::new(MemoryUsers::new(), hasher(8));
    #[allow(deprecated)]
    let shim = basic.clone().without_throttle_dangerous();
    assert!(shim.throttle.is_none());
    let open = basic.dangerous_without_throttle();
    assert!(open.throttle.is_none());
    let recorded = id_effect::observability::posture::dangerous_opt_outs();
    let (_, reopens) = recorded
      .iter()
      .find(|(id, _)| *id == DANGEROUS_WITHOUT_THROTTLE)
      .expect("recorded");
    assert!(reopens.starts_with("H5:"));
  }

  #[test]
  fn success_clears_the_account_and_a_custom_throttle_applies() {
    let h = hasher(8);
    let users = MemoryUsers::new();
    users.insert("alice", hash(&h, "pw"), AuthPrincipal::new("u1"));
    let ip = Some(IpAddr::from([198, 51, 100, 4]));
    let basic = BasicAuth::new(users, h).with_throttle(LoginThrottle::default().free_attempts(2));
    assert!(run(basic.authenticate_from("alice", "bad", ip)).is_err());
    assert!(run(basic.authenticate_from("alice", "pw", ip)).is_ok());
    // alice's count was cleared; the address keeps its one failure and locks at two.
    assert!(run(basic.authenticate_from("alice", "bad", None)).is_err());
    assert!(run(basic.authenticate_from("alice", "pw", None)).is_ok());
    assert_eq!(
      run(basic.authenticate_from("carol", "bad", ip)),
      Err(AuthError::InvalidCredentials)
    );
    assert!(matches!(
      run(basic.authenticate_encoded_from(
        basic_header("dave", "x").strip_prefix("Basic ").unwrap(),
        ip
      )),
      Err(AuthError::TooManyAttempts { .. })
    ));
  }

  #[test]
  fn l4_long_basic_password_rejected() {
    let h = hasher(8);
    let users = Counting::default();
    let long = "p".repeat(MAX_PASSWORD_LEN);
    users
      .users
      .insert("alice", hash(&h, &long), AuthPrincipal::new("u1"));
    let basic = BasicAuth::new(users.clone(), h);
    assert!(
      run(basic.authenticate("alice", long.as_str())).is_ok(),
      "256 bytes is fine"
    );
    // 129 two-byte characters: 258 bytes.
    for too_long in [format!("{long}p"), "é".repeat(129)] {
      assert!(matches!(
        run(basic.authenticate("alice", too_long.as_str())),
        Err(AuthError::Malformed(m)) if m.contains("256 bytes")
      ));
    }
    let encoded = basic_header("alice", &"x".repeat(10_000));
    assert!(matches!(
      run(basic.authenticate_encoded(encoded.strip_prefix("Basic ").unwrap())),
      Err(AuthError::Malformed(_))
    ));
    assert_eq!(users.lookups(), 1, "rejected before the lookup and hashing");
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
