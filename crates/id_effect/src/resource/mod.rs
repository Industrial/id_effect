//! **Stratum 8 — Resource Management**
//!
//! Safe acquisition and release of resources, built from Strata 0–7.
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`scope`] | [`Scope`], [`Finalizer`] | Stratum 12 (`stm::{TRef, commit}`), Stratum 9 (`latch::Latch`), Stratum 6 (`runtime`), Stratum 4 (`failure`) |
//! | [`pool`] | [`Pool`], [`KeyedPool`] | [`scope`], Stratum 9 (`coordination`) |
//! | [`cache`] | [`Cache`], [`CacheStats`] | Stratum 9 (`coordination`), Stratum 14 (`collections`) |
//!
//! ## Public API
//!
//! Re-exported at the crate root: [`Scope`], [`Finalizer`], [`Pool`], [`KeyedPool`], [`Cache`], [`CacheStats`].

pub mod cache;
pub mod pool;
pub mod scope;

pub use cache::{Cache, CacheStats};
pub use pool::{KeyedPool, Pool};
pub use scope::{Finalizer, Scope};
