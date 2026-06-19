//! Bridge between parser combinators and `id_effect::schema` (stub).

use crate::parser::Parser;
use id_effect::schema::{ParseError, Schema};

/// Placeholder for future `Schema` → parser derivation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaBridgeStub;

impl SchemaBridgeStub {
  /// Returns `None` until schema-driven parser generation lands.
  #[must_use]
  pub fn parser_for<A, I, E>(_: &Schema<A, I, E>) -> Option<Parser<String, A, ParseError>> {
    None
  }
}
