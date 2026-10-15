//! **Stratum 4 — Failure Algebra**
//!
//! Structured representation of computational failure, built entirely from Strata 0–3.
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`cause`] | [`Cause<E>`] ADT, [`Defect`] (panic payload), [`Semigroup`] impl | Stratum 0 (`option_`, `Matcher`), Stratum 1 (`Semigroup`), Stratum 6 bootstrap (`FiberId`) |
//! | [`exit`]  | [`Exit<A,E>`] terminal outcome | [`cause`] (this stratum), Stratum 0 (`Either`, `Matcher`) |
//! | [`union`] | [`Or<L,R>`] error union | Stratum 0 (`Either`) |
//! | [`fiber_id`] | [`FiberId`], the id an interrupt carries | `std::sync::atomic` only |
//! | [`never`] | [`Never`], the error type of infallible effects | — |
//!
//! ## Public API
//!
//! Re-exported at the crate root: [`Cause`], [`Defect`], [`Exit`], [`Or`], [`FiberId`], [`Never`].

pub mod cause;
pub mod exit;
pub mod fiber_id;
pub mod never;
pub mod pretty;
pub mod union;

pub use cause::{Cause, Defect};
pub use exit::Exit;
pub use fiber_id::FiberId;
pub use never::Never;
pub use pretty::{pretty_cause, pretty_cause_inline, pretty_exit, pretty_fiber_id};
pub use union::Or;
