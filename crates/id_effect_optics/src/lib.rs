//! Functional optics for [`id_effect`](https://docs.rs/id_effect) — **Part V ch18**.
//!
//! ## Contents
//!
//! - [`lens`] — total product focus (`get` / `set` / `modify` / `compose`)
//! - [`prism`] — partial sum-type focus
//! - [`optional`] — `Option` field helpers
//! - [`traversal`] — zero-or-many focus (vectors, [`im::Vector`], optional fields)
//! - [`iso`] — bidirectional isomorphisms
//! - [`transducer`] — composable reducing-function transforms
//! - [`path`] — dot-separated and JSON Pointer path parsing
//! - [`schema_bridge`] — path access on [`id_effect::schema::Unknown`]
//! - [`json_patch`] — RFC 6902 operations on [`Unknown`](id_effect::schema::Unknown)
//! - [`zipper`] — navigable trie zipper over persistent tries
//!
//! Pair with `#[derive(Optics)]` from `id_effect_proc_macro` for field/variant codegen.
//!
//! ## Book
//!
//! See the mdBook chapter *Optics with id_effect* (`ch18-00-optics.md`).

#![forbid(unsafe_code)]
#![allow(
  clippy::type_complexity,
  clippy::needless_borrow,
  clippy::redundant_closure,
  unused_mut
)]
#![deny(missing_docs)]
#![cfg_attr(
  test,
  allow(
    clippy::bool_assert_comparison,
    clippy::unwrap_used,
    clippy::expect_used
  )
)]

pub mod iso;
pub mod json_patch;
pub mod lens;
pub mod optional;
pub mod path;
pub mod prism;
pub mod schema_bridge;
pub mod transducer;
pub mod traversal;
pub mod zipper;

pub use iso::{Iso, identity_iso};
pub use json_patch::{PatchError, PatchOp, apply_patch, apply_patches};
pub use lens::{Lens, field, identity_lens};
pub use optional::Optional;
pub use path::{PathSegment, parse_path};
pub use prism::{Prism, some_prism};
pub use schema_bridge::{SchemaPathError, create_at_path, get_at_path, object, set_at_path};
pub use transducer::{Reducer, Transducer, filter, map, take};
pub use traversal::{Traversal, at_option, at_vec, im_vector_each, vector_each};
pub use zipper::{TrieNode, TrieZipper};
