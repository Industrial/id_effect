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
//! | [`parse`] | [`Schema`], [`ParseError`], [`Unknown`], primitives, `tuple`/`tuple3`/`tuple4`, `struct_`/`struct3`/`struct4`, … | [`data`] |
//! | [`extra`] | [`record`], [`suspend`], [`union_chain`], literals, [`wire_equal`](extra::wire_equal), [`null_or`](extra::null_or) | [`parse`] |
//! | [`parse_errors`] | [`ParseErrors`] | [`parse`] |
//! | [`has_schema`] | [`HasSchema`] | [`parse`] |
//! | [`serde_bridge`] | JSON → [`Unknown`] (`schema-serde` feature) | [`parse`], `serde_json` |
//! | [`json_schema_export`] | Primitive JSON Schema fragments (`schema-serde`) | `serde_json` |
//!
//! ## Public API
//!
//! Re-exported at the crate root: all public types and functions.

pub mod brand;
pub mod data;
pub mod equal;
pub mod extra;
pub mod has_schema;
pub mod order;
pub mod parse;
pub mod parse_errors;

#[cfg(feature = "schema-serde")]
pub mod json_schema_export;
#[cfg(feature = "schema-serde")]
pub mod serde_bridge;

pub use brand::{Brand, RefinedBrand};
pub use data::{DataError, DataStruct, DataTuple, EffectData};
pub use equal::{EffectHash, Equal, combine, equals, hash, hash_string, hash_structure};
pub use extra::{literal_i64, literal_string, null_or, record, suspend, union_chain, wire_equal};
pub use has_schema::HasSchema;
pub use order::{DynOrder, Ordering, ordering};
pub use parse::{
  ParseError, Schema, Unknown, array, bool_, f64, filter, i64, i64_unknown_wire, optional, refine,
  string, struct_, struct3, struct4, transform, tuple, tuple3, tuple4, union_,
};
pub use parse_errors::ParseErrors;

#[cfg(feature = "schema-serde")]
pub use json_schema_export::{
  type_array, type_boolean, type_integer, type_number, type_record, type_string,
};
#[cfg(feature = "schema-serde")]
pub use serde_bridge::unknown_from_serde_json;
