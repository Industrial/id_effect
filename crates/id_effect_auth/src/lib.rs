//! Authentication for id_effect services.
//!
//! - [`password`] — [`PasswordHasher`]: argon2id PHC hashes with the OWASP parameters by
//!   default, verification that hands back an upgraded hash when the parameters changed, and a
//!   dummy verification so an unknown user costs the same time as a wrong password. Hashing runs
//!   on the executor's blocking pool.
//! - [`session`] — [`Sessions`]: server-side sessions behind a [`SessionStore`] (in-memory
//!   [`MemorySessionStore`] for tests), with idle and absolute timeouts, a fresh id on every
//!   login, rotation, logout and revoke-all, and a hardened cookie. Feature `axum` adds
//!   `SessionLayer`, which loads the request's session into `id_effect_axum`'s request
//!   capabilities.
//!
//! Each is a capability with a [`Layer`](id_effect::Layer), like the rest of the platform crates.
//! Still to land in 0.9: bearer/JWT (JWKS rotation), basic and API-key credential layers
//! producing a principal, a database-backed session store, OIDC + PKCE and TOTP.
//!
//! Tier `integration` (ADR 0010 Amendment 5); `platform` once the Ledger reference app
//! under `examples/reference/` depends on it.
//!
//! ```
//! use id_effect::Exit;
//! use id_effect_auth::{
//!   MemorySessionStore, PasswordHasher, PasswordParams, SessionConfig, Sessions, Verification,
//! };
//!
//! let hasher = PasswordHasher::new(PasswordParams::new(64, 1, 1).unwrap()); // low cost for docs
//! let sessions = Sessions::new(MemorySessionStore::new(), SessionConfig::default());
//!
//! let Exit::Success(stored) = hasher.hash("s3cret").block_on() else { panic!() };
//! let Exit::Success(check) = hasher.verify_or_dummy("s3cret", Some(&stored)).block_on() else {
//!   panic!()
//! };
//! assert_eq!(check, Verification::Valid);
//! let Exit::Success(session) = sessions.login("user-1", None).block_on() else { panic!() };
//! assert!(sessions.set_cookie(&session).contains("HttpOnly"));
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod password;
pub mod session;

pub use password::{
  Password, PasswordError, PasswordHash, PasswordHasher, PasswordHasherEnv, PasswordParams,
  Verification,
};
#[cfg(feature = "axum")]
pub use session::SessionLayer;
pub use session::{
  CookieConfig, MemorySessionStore, SameSite, Session, SessionConfig, SessionError, SessionId,
  SessionStore, SessionStoreService, Sessions,
};

/// Compiles and runs the README's code block as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
