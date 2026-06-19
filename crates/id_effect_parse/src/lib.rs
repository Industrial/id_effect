//! Parser combinators, pretty printing, invertible codecs, and diffs for
//! [`id_effect`](https://docs.rs/id_effect).
//!
//! ## Modules
//!
//! - [`parser`] — [`Parser`] with `map`, `and_then`, `alt`, `many`
//! - [`byte`] — byte-buffer parsers
//! - [`json`] — JSON text → [`id_effect::schema::Unknown`]
//! - [`pretty`] — [`Doc`] documents and the [`Pretty`] trait
//! - [`codec`] — invertible parse/print [`Codec`]
//! - [`diff`] — [`Diff`] for value comparisons
//! - [`effect_bridge`] — parse from [`id_effect::Stream`] chunks
//! - [`schema_bridge`] — [`Schema`] ↔ text [`Parser`] bridge
//!
//! ## Book
//!
//! See mdBook Part V chapter 20 (`ch20-00-parser-combinators.md`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod byte;
pub mod codec;
pub mod diff;
pub mod effect_bridge;
pub mod json;
pub mod parser;
pub mod pretty;
pub mod schema_bridge;

pub use byte::{byte, byte_int, byte_tag, byte_ws, parse_bytes};
pub use codec::quoted_string;
pub use codec::{Codec, bool_codec, float_codec, int_codec, list, pair};
pub use diff::{Diff, apply_diff, diff_option, diff_values};
pub use effect_bridge::{ParseStreamError, parse_stream, parse_text_stream};
pub use json::{parse_json_document, parse_json_value};
pub use parser::{
  ParseFailure, Parser, between, bool_lit, char, float, int, many1, optional, parse_all, parse_str,
  sep_by, signed_int, tag, void, ws,
};
pub use pretty::{Doc, Pretty};
pub use schema_bridge::{SchemaBridge, SchemaBridgeStub};
