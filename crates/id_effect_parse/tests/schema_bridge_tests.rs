use id_effect::schema::{HasSchema, ParseError, Schema, Unknown, i64, string, struct_};
use id_effect_parse::{SchemaBridge, parse_json_document};

#[test]
fn schema_bridge_parses_json_struct() {
  let schema = struct_("name", string::<()>(), "age", i64::<()>());
  let parser = SchemaBridge::parser_for_json(schema);
  let (value, rest) = parser
    .parse(r#"{"name":"Ada","age":36}"#.to_string())
    .unwrap();
  assert_eq!(value, ("Ada".to_string(), 36_i64));
  assert!(rest.is_empty());
}

#[test]
fn schema_bridge_string_wire() {
  let parser = SchemaBridge::parser_for_string_wire(string::<()>());
  let (value, _) = parser.parse("hello".to_string()).unwrap();
  assert_eq!(value, "hello");
}

struct Profile;

impl HasSchema for Profile {
  type A = (String, i64);
  type I = (String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I, Self::E> {
    struct_("name", string(), "score", i64())
  }
}

#[test]
fn schema_bridge_has_schema_helper() {
  let parser = SchemaBridge::parser_for::<Profile>();
  let (value, _) = parser
    .parse(r#"{"name":"Grace","score":99}"#.to_string())
    .unwrap();
  assert_eq!(value, ("Grace".to_string(), 99));
}

#[test]
fn json_module_parses_document() {
  let doc = parse_json_document(r#"{"ok":true}"#).unwrap();
  assert!(matches!(doc, Unknown::Object(_)));
}

#[test]
fn json_errors_are_parse_error() {
  let err = parse_json_document("not-json").unwrap_err();
  assert!(matches!(err, ParseError { .. }));
}
