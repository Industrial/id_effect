//! Integration-test support for id_effect programs.
//!
//! **Status: in development for 0.9.** The crate is registered (workspace member, publish
//! order, CI) but has no public items yet; they land with the 0.9 leaves that own them
//! (E6.5). Planned scope:
//!
//! - effect-native containers (Postgres, MySQL, Redis, Keycloak, Vault) as scoped layers
//! - per-test database isolation (rolled-back transaction or template-database clone)
//! - fixtures/factories and the in-process HTTP client
//!
//! Container tests run in CI's containers job with `ID_EFFECT_REQUIRE_DOCKER=1`.
//!
//! Tier `integration` (ADR 0010 Amendment 5); `platform` once the Ledger reference app
//! under `examples/reference/` depends on it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Compiles and runs the README's code block as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
