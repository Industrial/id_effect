//! Parser combinators, pretty printing, invertible codecs, and diffs for
//! [`id_effect`](https://docs.rs/id_effect).
//!
//! ## Modules
//!
//! - [`parser`] — [`Parser`] with `map`, `and_then`, `alt`, `many`
//! - [`pretty`] — [`Doc`] documents and the [`Pretty`] trait
//! - [`codec`] — invertible parse/print [`Codec`]
//! - [`diff`] — [`Diff`] for value comparisons
//! - [`effect_bridge`] — parse from [`id_effect::Stream`] chunks
//! - [`schema_bridge`] — future `Schema` integration (stub)
//!
//! ## Book
//!
//! See mdBook Part V chapter 20 (`ch20-00-parser-combinators.md`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod codec;
pub mod diff;
pub mod effect_bridge;
pub mod parser;
pub mod pretty;
pub mod schema_bridge;

pub use codec::Codec;
pub use diff::{Diff, apply_diff, diff_option, diff_values};
pub use effect_bridge::{ParseStreamError, parse_stream, parse_text_stream};
pub use parser::{ParseFailure, Parser, char, int, parse_all, parse_str, tag, ws};
pub use pretty::{Doc, Pretty};
pub use schema_bridge::SchemaBridgeStub;
