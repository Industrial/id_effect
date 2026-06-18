//! Functional optics for [`id_effect`](https://docs.rs/id_effect) — **Part V ch18**.
//!
//! ## Contents
//!
//! - [`lens`] — total product focus (`get` / `set` / `modify` / `compose`)
//! - [`prism`] — partial sum-type focus
//! - [`optional`] — `Option` field helpers
//! - [`traversal`] — zero-or-many focus (vectors, [`im::Vector`])
//! - [`transducer`] — composable reducing-function transforms
//! - [`schema_bridge`] — dot-path access on [`id_effect::schema::Unknown`]
//! - [`json_patch`] — RFC 6902 subset (`add`, `replace`, `remove`)
//! - [`zipper`] — trie zipper stub for immutable navigation
//!
//! ## Book
//!
//! See the mdBook chapter *Optics with id_effect* (`ch18-00-optics.md`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(
  test,
  allow(
    clippy::bool_assert_comparison,
    clippy::unwrap_used,
    clippy::expect_used
  )
)]

pub mod json_patch;
pub mod lens;
pub mod optional;
pub mod prism;
pub mod schema_bridge;
pub mod transducer;
pub mod traversal;
pub mod zipper;

pub use json_patch::{PatchError, PatchOp, apply_patch, apply_patches};
pub use lens::{Lens, field, identity_lens};
pub use optional::Optional;
pub use prism::{Prism, some_prism};
pub use schema_bridge::{SchemaPathError, get_at_path, object, set_at_path};
pub use transducer::{Reducer, Transducer, filter, map};
pub use traversal::{Traversal, im_vector_each, vector_each};
pub use zipper::{TrieNode, TrieZipper};
