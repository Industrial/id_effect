//! Authorization for id_effect services.
//!
//! **Status: in development for 0.9.** The crate is registered (workspace member, publish
//! order, CI) but has no public items yet; they land with the 0.9 leaves that own them
//! (E5.3). Planned scope:
//!
//! - role and permission sets, named policies, resource/ownership and attribute policies
//! - route guards and an allow/deny event stream
//! - test helpers that assert a policy allows or denies
//!
//! It defines the `Principal` trait that `id_effect_auth` implements.
//!
//! Tier `integration` (ADR 0010 Amendment 5); `platform` once the Ledger reference app
//! under `examples/reference/` depends on it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Compiles and runs the README's code block as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
