//! Compile-fail security cases for id_effect (ADR 0013): each case is a program that misuses a
//! secure-by-default API and must be rejected by the compiler. The cases live under `ui/`, run by
//! `tests/compile_fail.rs` and listed with their rule in `docs/security/compile-fail.md`.
//!
//! This crate is `publish = false` and is never in the publish order (ADR 0010 Amendment 7).

#![deny(missing_docs)]
