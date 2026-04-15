//! **Stratum 1: Algebraic Structures** — abstract patterns that recur throughout the system.
//!
//! This module provides the fundamental algebraic abstractions built on top of
//! [Stratum 0 foundations](super::foundation). These structures capture common
//! patterns of composition, transformation, and combination.
//!
//! ## Hierarchy
//!
//! ```text
//!                    Semigroup
//!                        │
//!                        ▼
//!                     Monoid
//!
//!     Contravariant   Functor   Bifunctor
//!                        │
//!                        ▼
//!                   Applicative
//!                        │
//!                        ▼
//!                      Monad
//! ```
//!
//! ## Design Notes
//!
//! Rust lacks higher-kinded types, so we use two complementary approaches:
//!
//! 1. **Traits with associated types** — for types with a single "mappable" parameter
//! 2. **Module functions** — for operations on concrete types (like `Option`, `Result`)
//!
//! The traits express the *structure*, while module functions provide ergonomic usage.

pub mod applicative;
pub mod bifunctor;
pub mod contravariant;
pub mod functor;
pub mod interface;
pub mod monad;
pub mod monoid;
pub mod selective;
pub mod semigroup;

pub use applicative::Applicative;
pub use bifunctor::Bifunctor;
pub use contravariant::Contravariant;
pub use functor::Functor;
pub use monad::Monad;
pub use monoid::Monoid;
pub use selective::Selective;
pub use semigroup::Semigroup;
