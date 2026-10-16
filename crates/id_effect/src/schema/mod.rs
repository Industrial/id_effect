//! **Stratum 13 — Data & Schema**
//!
//! Type-safe data representation and validation, built from Strata 0–12.
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`brand`] | [`Brand`], [`RefinedBrand`] | Stratum 0 |
//! | [`equal`] | [`Equal`], [`EffectHash`] | Stratum 0 |
//! | [`data`] | [`EffectData`], [`DataStruct`], [`DataTuple`], [`DataError`] | [`equal`] |
//! | [`order`] | [`DynOrder`], [`Ordering`], `ordering`, `order` | Stratum 0 |
//! | [`ast`] | [`SchemaAst`], [`SchemaKind`], [`Constraints`], [`Annotations`] — introspectable wire shape | Stratum 0 |
//! | [`parse`] | [`Schema`], [`ParseError`], [`Unknown`], primitives, `tuple`/`tuple3`..`tuple12`, `struct_`/`struct3`..`struct12`, … | [`data`], [`ast`] |
//! | [`extra`] | [`record`], [`suspend`], [`suspend_named`], [`union_chain`], literals, [`wire_equal`](extra::wire_equal), [`null_or`](extra::null_or) | [`parse`] |
//! | [`validators`] | [`min_length`], [`max_length`], [`pattern`], `regex` (`schema-regex`), [`email`], [`uuid`], [`url`], [`one_of`], [`range`], [`min_items`], … | [`parse`] |
//! | [`parse_errors`] | [`ParseErrors`] | [`parse`] |
//! | [`has_schema`] | [`HasSchema`], [`ast_ref`] | [`parse`] |
//! | [`serde_bridge`] | JSON → [`Unknown`] (`schema-serde` feature) | [`parse`], `serde_json` |
//! | `json_schema_export` | JSON Schema 2020-12 export: `to_json_schema`, `json_schema_for`, `Schema::json_schema` (`schema-serde`) | [`ast`], `serde_json` |
//!
//! ## Public API
//!
//! Re-exported at the crate root: all public types and functions.

pub mod ast;
pub mod brand;
pub mod data;
pub mod equal;
pub mod extra;
pub mod has_schema;
pub mod order;
pub mod parse;
pub mod parse_errors;
pub mod redacted;
pub mod validators;

#[cfg(feature = "schema-serde")]
pub mod json_schema_export;
#[cfg(feature = "schema-serde")]
pub mod serde_bridge;

pub use ast::{Annotations, Constraints, Field, LazyAst, SchemaAst, SchemaKind};
pub use brand::{Brand, RefinedBrand};
pub use data::{DataError, DataStruct, DataTuple, EffectData};
pub use equal::{EffectHash, Equal, combine, equals, hash, hash_string, hash_structure};
pub use extra::{
  literal_i64, literal_string, null_or, record, suspend, suspend_named, union_chain, wire_equal,
};
pub use has_schema::{HasSchema, ast_ref};
pub use order::{DynOrder, Ordering, ordering};
pub use parse::{
  ParseError, Schema, Unknown, array, bool_, f64, filter, i64, i64_unknown_wire, optional, refine,
  string, struct_, struct3, struct4, struct5, struct6, struct7, struct8, struct9, struct10,
  struct11, struct12, transform, tuple, tuple3, tuple4, tuple5, tuple6, tuple7, tuple8, tuple9,
  tuple10, tuple11, tuple12, union_,
};
pub use parse_errors::ParseErrors;
pub use redacted::Redacted;
#[cfg(feature = "schema-regex")]
pub use validators::regex;
pub use validators::{
  NumericBound, email, exclusive_maximum, exclusive_minimum, length, max_items, max_length,
  maximum, min_items, min_length, minimum, non_empty, one_of, pattern, range, url, uuid,
};

#[cfg(feature = "schema-serde")]
pub use json_schema_export::{
  ast_to_json_schema, json_schema_for, to_json_schema, type_array, type_boolean, type_integer,
  type_number, type_record, type_string,
};
#[cfg(feature = "schema-serde")]
pub use serde_bridge::{serde_json_from_unknown, unknown_from_serde_json};
