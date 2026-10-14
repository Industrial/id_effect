//! Functional-programming toolkit for [`id_effect`] (**maintenance tier**, ADR 0010).
//!
//! These modules used to live in the core `id_effect` crate. They are kept working and tested,
//! but they are not on the core roadmap; the core runtime does not depend on them.
//!
//! - **[`mod@algebra`]** — type-class-shaped traits ([`Semigroup`], [`Monoid`], [`Functor`],
//!   [`Monad`], [`Foldable`], [`traverse_vec`], …) with instances for `Option`, `Result`, `Vec`,
//!   [`id_effect::Effect`] and [`id_effect::Cause`].
//! - **[`mod@foundation`]** — categorical constructs core does not need: [`Unit`], [`Never`],
//!   [`Iso`], products, coproducts, and `Option` helpers. (`Either`, `Pipe`, `Predicate`,
//!   `MutableRef` and `func` stay in `id_effect::foundation`.)
//! - **[`mod@collections`]** — persistent collections backed by [`im`] ([`EffectHashMap`],
//!   [`EffectVector`], …) plus mutable helpers ([`MutableList`], [`ChunkBuilder`], …).
//! - **[`mod@stm`]** — software transactional memory ([`TRef`], [`TMap`], [`commit`], …).
//! - **[`mod@result`]**, **[`mod@reader`]**, **[`mod@thunk`]** — pure `Result` combinators, the
//!   `Reader` monad and one-shot thunks (formerly `id_effect::kernel::{result, reader, thunk}`).
//!
//! ```rust
//! use id_effect::run_blocking;
//! use id_effect_fp::{Semigroup, TRef, commit, hash_map};
//!
//! let counter = run_blocking(commit(TRef::make(1_i32)), ()).expect("make");
//! run_blocking(commit(counter.update_stm::<(), _>(|n| n + 1)), ()).expect("update");
//! assert_eq!(run_blocking(commit(counter.read_stm::<()>()), ()), Ok(2));
//!
//! let m = hash_map::set(&hash_map::empty(), "a", 1);
//! assert_eq!(hash_map::get(&m, "a"), Some(&1));
//! assert_eq!(vec![1, 2].combine(vec![3]), vec![1, 2, 3]);
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(rustdoc::all)]
#![allow(
  clippy::doc_overindented_list_items,
  clippy::should_implement_trait,
  clippy::type_complexity,
  clippy::unnecessary_lazy_evaluations
)]
#![cfg_attr(
  test,
  allow(
    clippy::bool_assert_comparison,
    clippy::clone_on_copy,
    clippy::empty_line_after_doc_comments,
    clippy::let_unit_value,
    clippy::map_identity,
    clippy::redundant_closure,
    clippy::unit_arg,
    clippy::unit_cmp,
  )
)]

pub mod algebra;
pub mod collections;
pub mod foundation;
pub mod reader;
pub mod result;
pub mod stm;
pub mod thunk;

/// Re-export of [`im`]: `id_effect_fp::im::Vector` etc. share one `im` version with
/// [`mod@collections`] (whose persistent types are aliases of `im` types).
pub use im;

pub use algebra::{
  Alternative, Applicative, Bifunctor, Contravariant, Foldable, FreeAp, Functor, Invariant, Monad,
  Monoid, Selective, Semigroup, sequence_vec, traverse_option, traverse_vec,
};
pub use collections::{
  ChunkBuilder, EffectHashMap, EffectHashSet, EffectSortedMap, EffectSortedSet, EffectVector,
  MutableHashMap, MutableHashSet, MutableList, MutableQueue, RedBlackTree, Trie, hash_map,
  hash_set, sorted_map, sorted_set, vector,
};
pub use foundation::{Iso, Never, Unit};
pub use stm::{Outcome, Stm, TMap, TQueue, TRef, TSemaphore, Txn, atomically, commit};
