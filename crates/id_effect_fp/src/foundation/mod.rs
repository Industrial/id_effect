//! **Stratum 0: Foundations** — categorical constructs the core runtime does not need.
//!
//! The constructs core *does* use (`Either`, `func`, `Pipe`, `Predicate`, `MutableRef`) stay in
//! `id_effect::foundation`; this module holds the rest.
//!
//! | Module | Construct | Category Theory |
//! |--------|-----------|-----------------|
//! | [`unit`] | `()` | Terminal object |
//! | [`never`] | `Never` | Initial object |
//! | [`function`] | `identity`, `compose`, `const_` | Morphisms |
//! | [`product`] | `(A, B)`, `fst`, `snd`, `pair` | Categorical product |
//! | [`coproduct`] | `Either<L, R>`, `left`, `right` | Categorical coproduct |
//! | [`isomorphism`] | `Iso<A, B>` | Isomorphic objects |
//! | [`option_`] | Free functions over `Option<T>` | — |
//!
//! ## Laws
//!
//! - **Identity**: `compose(f, identity) ≡ f ≡ compose(identity, f)`
//! - **Associativity**: `compose(f, compose(g, h)) ≡ compose(compose(f, g), h)`

pub mod coproduct;
pub mod function;
pub mod isomorphism;
pub mod never;
pub mod option_;
pub mod product;
pub mod unit;

pub use coproduct::{Either, either, left, right};
pub use function::{absurd, always, compose, const_, flip, identity, pipe1, pipe2, pipe3};
pub use isomorphism::Iso;
pub use never::Never;
pub use product::{bimap_product, fst, pair, snd, swap};
pub use unit::Unit;
