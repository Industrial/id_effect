//! **Stratum 0: Foundations** — the categorical bedrock upon which all effect abstractions rest.
//!
//! This module provides the primitive mathematical constructs that form the basis of the
//! effect system. Every higher-level abstraction (functors, monads, effects, streams) is
//! ultimately built from these foundations.
//!
//! ## Core Constructs
//!
//! | Module | Construct | Category Theory |
//! |--------|-----------|-----------------|
//! | [`unit`] | `()` | Terminal object |
//! | [`never`] | `Never` | Initial object |
//! | [`function`] | `identity`, `compose`, `const_` | Morphisms |
//! | [`product`] | `(A, B)`, `fst`, `snd`, `pair` | Categorical product |
//! | [`coproduct`] | `Either<L, R>`, `left`, `right` | Categorical coproduct |
//! | [`isomorphism`] | `Iso<A, B>` | Isomorphic objects |
//!
//! ## Practical Utilities (Effect.ts mirrors)
//!
//! | Module | Construct |
//! |--------|-----------|
//! | [`either`] | `Either<R,L>` alias + Effect.ts-named combinators |
//! | [`func`] | `identity`, `compose`, `memoize`, `tupled` (full set) |
//! | [`option_`] | Free functions over `Option<T>` |
//! | [`piping`] | [`Pipe`] trait (`x.pipe(f)`) |
//! | [`predicate`] | [`Predicate<A>`] composable boolean functions |
//! | [`mutable_ref`] | [`MutableRef<A>`] synchronous interior-mutable cell |
//!
//! ## Laws
//!
//! - **Identity**: `compose(f, identity) ≡ f ≡ compose(identity, f)`
//! - **Associativity**: `compose(f, compose(g, h)) ≡ compose(compose(f, g), h)`

pub mod coproduct;
pub mod either;
pub mod func;
pub mod function;
pub mod isomorphism;
pub mod mutable_ref;
pub mod never;
pub mod option_;
pub mod piping;
pub mod predicate;
pub mod product;
pub mod unit;

pub use coproduct::{Either, either, left, right};
pub use function::{absurd, always, compose, const_, flip, identity, pipe1, pipe2, pipe3};
pub use isomorphism::Iso;
pub use mutable_ref::MutableRef;
pub use never::Never;
pub use piping::Pipe;
pub use predicate::Predicate;
pub use product::{bimap_product, fst, pair, snd, swap};
pub use unit::Unit;
