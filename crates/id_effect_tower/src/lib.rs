//! **Deprecated (0.6):** merged into [`id_effect_axum::service`] (enable `id_effect_axum`'s
//! `tower` feature). This crate re-exports it for one release and will be removed in 0.7.
//! See ADR 0010 (`docs/adrs/0010-crate-tiers.md`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub use id_effect_axum::service::*;
