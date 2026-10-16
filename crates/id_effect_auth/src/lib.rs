//! Authentication for id_effect services.
//!
//! **Status: in development for 0.9.** The crate is registered (workspace member, publish
//! order, CI) but has no public items yet; they land with the 0.9 leaves that own them
//! (E5.2a, E5.2b). Planned scope:
//!
//! - password hashing (argon2id) and persistent sessions
//! - bearer/JWT (with JWKS rotation), basic and API-key credential layers producing a principal
//! - OIDC authorization code + PKCE, TOTP second factor
//!
//! Its principal implements `id_effect_authz`'s `Principal`, so it publishes after that crate.
//!
//! Tier `integration` (ADR 0010 Amendment 5); `platform` once the Ledger reference app
//! under `examples/reference/` depends on it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Compiles and runs the README's code block as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
