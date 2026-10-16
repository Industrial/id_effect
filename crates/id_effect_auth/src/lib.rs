//! Authentication for id_effect services.
//!
//! - [`password`] — [`PasswordHasher`]: argon2id PHC hashes with the OWASP parameters by
//!   default, verification that hands back an upgraded hash when the parameters changed, and a
//!   dummy verification so an unknown user costs the same time as a wrong password. Hashing runs
//!   on the executor's blocking pool.
//! - [`session`] — [`Sessions`]: server-side sessions behind a [`SessionStore`] (in-memory
//!   [`MemorySessionStore`] for tests), with idle and absolute timeouts, a fresh id on every
//!   login, rotation, logout and revoke-all, and a hardened cookie. Feature `pg` adds
//!   `PgSessionStore`: sessions in PostgreSQL over `id_effect_sql`, keyed by the SHA-256 of the
//!   id, with the crate's `migrations/`. Feature `axum` adds `SessionLayer`, which loads the
//!   request's session into `id_effect_axum`'s request capabilities.
//! - [`principal`] — [`AuthPrincipal`], the `id_effect_authz::Principal` every credential check
//!   yields, tagged with its [`AuthScheme`]; [`AuthError`] says why a credential was rejected.
//! - [`apikey`] — [`ApiKeys`]: random keys stored as SHA-256 digests in an [`ApiKeyStore`],
//!   found by public id and compared in constant time.
//! - [`basic`] — [`BasicAuth`]: username and password against a [`UserLookup`], with
//!   [`PasswordHasher::verify_or_dummy`] for unknown users.
//! - `jwt` (feature `jwt`) — `JwtVerifier` / `JwtIssuer`: HS/RS/PS/ES/EdDSA tokens with issuer,
//!   audience, expiry and required-claim checks and a claim → roles/permissions mapping.
//!   Feature `jwks` adds `JwksClient`: the identity provider's key set, cached and refetched
//!   (rate-limited) when a token names an unknown `kid`.
//! - `middleware` (feature `axum`) — `Authenticate`: session → bearer → API key → Basic, in a
//!   configurable order, installing the principal as `id_effect_authz::CurrentPrincipal`.
//!   Missing credentials continue anonymously; invalid ones answer 401 Problem Details.
//! - `oidc` (feature `oidc`) — `OidcClient`: OpenID Connect discovery, authorization code +
//!   PKCE (S256), ID tokens verified over the provider's JWKS (`iss`, `aud`, `exp`, `nonce`,
//!   `azp`) and mapped with a `ClaimMapping`; `OidcRoutes`: `/auth/oidc/{login,callback,logout}`
//!   with state, nonce and code verifier in a server-side flow session, login through
//!   `Sessions::login`, and RP-initiated logout. Implements id_effect_platform's `OAuthClient`.
//! - `totp` (feature `totp`) — `Totp`: RFC 6238 enrolment (160-bit secret, `otpauth://` URI)
//!   and verification with ±1 step of skew on a `WallClock` and replay protection through a
//!   `TotpStepStore`; `RecoveryCodes`: single-use codes stored as argon2id hashes;
//!   `Totp::step_up` marks the session after a second factor, `with_mfa_claims` and
//!   `require_mfa` carry it to `id_effect_authz` policies.
//!
//! [`PasswordHasher`] and [`Sessions`] are capabilities with a [`Layer`](id_effect::Layer), like
//! the rest of the platform crates; the credential checks are cheap-to-clone values whose methods
//! return effects, held by the middleware or a handler.
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

pub mod apikey;
pub mod basic;
mod error;
#[cfg(feature = "jwks")]
pub mod jwks;
#[cfg(feature = "jwt")]
pub mod jwt;
#[cfg(feature = "axum")]
pub mod middleware;
#[cfg(feature = "oidc")]
pub mod oidc;
pub mod password;
pub mod principal;
pub mod session;
#[cfg(feature = "totp")]
pub mod totp;

pub use apikey::{ApiKey, ApiKeyRecord, ApiKeyStore, ApiKeys, MemoryApiKeyStore};
pub use basic::{
  BasicAuth, MemoryUsers, UserCredentials, UserLookup, basic_credentials, basic_header,
};
pub use error::AuthError;
#[cfg(feature = "jwks")]
pub use jwks::JwksClient;
#[cfg(feature = "jwt")]
pub use jwt::{Algorithm, ClaimMapping, JwtIssuer, JwtKey, JwtVerifier};
#[cfg(feature = "axum")]
pub use middleware::{Authenticate, AuthenticateService};
#[cfg(feature = "oidc")]
pub use oidc::{OidcClient, OidcConfig, OidcError, OidcRoutes};
pub use password::{
  Password, PasswordError, PasswordHash, PasswordHasher, PasswordHasherEnv, PasswordParams,
  Verification,
};
pub use principal::{AuthPrincipal, AuthScheme};
#[cfg(feature = "axum")]
pub use session::SessionLayer;
pub use session::{
  CookieConfig, MemorySessionStore, SameSite, Session, SessionConfig, SessionError, SessionId,
  SessionStore, SessionStoreService, Sessions,
};
#[cfg(feature = "pg")]
pub use session::{PgSessionStore, pg_session_store_layer};
#[cfg(feature = "totp")]
pub use totp::{
  MemoryTotpStepStore, RecoveryCodes, Totp, TotpConfig, TotpError, TotpSecret, TotpStepStore,
  TotpVerification,
};

/// Compiles and runs the README's code block as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
