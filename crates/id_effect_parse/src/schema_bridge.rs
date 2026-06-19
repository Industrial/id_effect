//! Bridge between parser combinators and [`id_effect::schema`].

use crate::json::{parse_json_document, parse_json_value};
use crate::parser::Parser;
use id_effect::schema::{HasSchema, ParseError, Schema};

/// Connects [`Schema`] values to text [`Parser`]s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchemaBridge;

impl SchemaBridge {
  /// Wrap a [`Schema`] whose wire type is [`String`]; consumes the entire input.
  #[must_use]
  pub fn parser_for_string_wire<A, E>(schema: Schema<A, String, E>) -> Parser<String, A, ParseError>
  where
    A: Send + Sync + 'static,
    E: Send + Sync + 'static,
  {
    Parser::new(move |input: String| match schema.decode(input.clone()) {
      Ok(value) => Ok((value, String::new())),
      Err(err) => Err(err),
    })
  }

  /// Parse JSON text, then run [`Schema::decode_unknown`].
  #[must_use]
  pub fn parser_for_json<A, I, E>(schema: Schema<A, I, E>) -> Parser<String, A, ParseError>
  where
    A: Send + Sync + 'static,
    I: Send + Sync + 'static,
    E: Send + Sync + 'static,
  {
    Parser::new(move |input: String| {
      let unknown = parse_json_document(&input)?;
      let value = schema.decode_unknown(&unknown)?;
      Ok((value, String::new()))
    })
  }

  /// Parse JSON text, returning leftover suffix after the first value.
  #[must_use]
  pub fn parser_for_json_prefix<A, I, E>(schema: Schema<A, I, E>) -> Parser<String, A, ParseError>
  where
    A: Send + Sync + 'static,
    I: Send + Sync + 'static,
    E: Send + Sync + 'static,
  {
    Parser::new(move |input: String| {
      let (unknown, rest) = parse_json_value(&input)?;
      let value = schema.decode_unknown(&unknown)?;
      Ok((value, rest))
    })
  }

  /// Build a parser for any type that exposes [`HasSchema`].
  #[must_use]
  pub fn parser_for<T>() -> Parser<String, T::A, ParseError>
  where
    T: HasSchema + Send + Sync + 'static,
    T::A: Send + Sync + 'static,
    T::I: Send + Sync + 'static,
    T::E: Send + Sync + 'static,
  {
    Self::parser_for_json(T::schema())
  }
}

/// Backward-compatible alias for [`SchemaBridge`].
pub type SchemaBridgeStub = SchemaBridge;

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::schema::{HasSchema, i64, string, struct_};

  struct PersonSchema;

  impl HasSchema for PersonSchema {
    type A = (String, i64);
    type I = (String, i64);
    type E = ();

    fn schema() -> Schema<Self::A, Self::I, Self::E> {
      struct_("name", string(), "age", i64())
    }
  }

  #[test]
  fn json_wire_parses_struct_schema() {
    let parser = SchemaBridge::parser_for_json(PersonSchema::schema());
    let (value, rest) = parser
      .parse(r#"{"name":"Ada","age":36}"#.to_string())
      .unwrap();
    assert_eq!(value, ("Ada".to_string(), 36));
    assert!(rest.is_empty());
  }

  #[test]
  fn string_wire_parses_whole_input() {
    let parser = SchemaBridge::parser_for_string_wire(string::<()>());
    let (value, rest) = parser.parse("hello".to_string()).unwrap();
    assert_eq!(value, "hello");
    assert!(rest.is_empty());
  }

  #[test]
  fn has_schema_helper_builds_parser() {
    let parser = SchemaBridge::parser_for::<PersonSchema>();
    let (value, _) = parser
      .parse(r#"{"name":"Grace","age":90}"#.to_string())
      .unwrap();
    assert_eq!(value.0, "Grace");
  }
}
