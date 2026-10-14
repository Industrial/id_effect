//! **Stratum 0: Foundations** — the small set of pure helpers the core runtime is built on.
//!
//! The categorical constructs core does not use (`Unit`, `Never`, `Iso`, products, coproducts,
//! `function`, `option_`) live in the maintenance-tier `id_effect_fp` crate
//! (`id_effect_fp::foundation`).
//!
//! | Module | Construct |
//! |--------|-----------|
//! | [`either`] | `Either<R,L>` alias + Effect.ts-named combinators |
//! | [`func`] | `identity`, `compose`, `always`, `absurd` |
//! | [`piping`] | [`Pipe`] trait (`x.pipe(f)`) |
//! | [`predicate`] | [`Predicate<A>`] composable boolean functions |
//!
//! ## Laws
//!
//! - **Identity**: `compose(f, identity) ≡ f ≡ compose(identity, f)`
//! - **Associativity**: `compose(f, compose(g, h)) ≡ compose(compose(f, g), h)`

pub mod either;
pub mod func;
pub mod piping;
pub mod predicate;

pub use either::Either;
pub use either::either::{left, right};
pub use func::{always, compose, identity};
pub use piping::Pipe;
pub use predicate::Predicate;
