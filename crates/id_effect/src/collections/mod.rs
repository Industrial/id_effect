//! **Stratum 14 — Collections**
//!
//! Persistent and mutable data structures built from Strata 0–13.
//!
//! All persistent types are backed by the [`im`] crate (re-exported as
//! `id_effect::im`).  The free functions mirror Effect.ts naming so that code can
//! be written in the same style as the TypeScript effect system.
//!
//! ## Persistent collections (backed by `im`)
//!
//! | Submodule | Type alias | Underlying `im` type | Notes |
//! |-----------|-----------|----------------------|-------|
//! | [`hash_map`] | [`EffectHashMap`] | `im::HashMap` | HAMT, O(1) avg |
//! | [`hash_set`] | [`EffectHashSet`] | `im::HashSet` | HAMT, O(1) avg |
//! | [`sorted_map`] | [`EffectSortedMap`] | `im::OrdMap` | B-tree, O(log n) |
//! | [`sorted_set`] | [`EffectSortedSet`] | `im::OrdSet` | B-tree, O(log n) |
//! | [`vector`] | [`EffectVector`] | `im::Vector` | RRB-tree, O(log n) |
//! | [`red_black_tree`] | [`RedBlackTree`] | `im::OrdMap` multimap | Ordered multi-map |
//!
//! ## Mutable collections (stdlib-backed, `Mutex`-guarded)
//!
//! | Submodule | Type | Notes |
//! |-----------|------|-------|
//! | [`mutable_list`] | [`MutableList`], [`ChunkBuilder`] | Deque with shared `append`/`prepend` |
//! | [`mutable_queue`] | [`MutableQueue`] | Bounded/unbounded FIFO |
//! | [`trie`] | [`Trie`] | Prefix-tree, `str`-keyed |

pub mod hash_map;
pub mod hash_set;
pub mod mutable_list;
pub mod mutable_queue;
pub mod red_black_tree;
pub mod sorted_map;
pub mod sorted_set;
pub mod trie;
pub mod vector;

pub use hash_map::{EffectHashMap, MutableHashMap};
pub use hash_set::{EffectHashSet, MutableHashSet};
pub use mutable_list::{ChunkBuilder, MutableList};
pub use mutable_queue::MutableQueue;
pub use red_black_tree::RedBlackTree;
pub use sorted_map::EffectSortedMap;
pub use sorted_set::EffectSortedSet;
pub use trie::Trie;
pub use vector::EffectVector;
