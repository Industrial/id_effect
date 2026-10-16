//! Golden tests: JSON Schema (2020-12) export of `#[derive(Schema)]` types (data.jsonschema).
//!
//! The derive attaches a `SchemaAst` built from each field's schema, so `json_schema_for::<T>()`
//! describes the struct instead of `{}`. Nested `HasSchema` fields are named lazy references
//! (`ast_ref`), exported once under `$defs` keyed by type path. `derive(Schema)` supports structs
//! with named fields only (no enums), so there is no enum case.

#![cfg(feature = "schema-serde")]

use id_effect::schema::json_schema_export::{DIALECT, json_schema_for};
use id_effect::schema::{
  Field, HasSchema, Schema, SchemaAst, SchemaKind, Unknown, i64_unknown_wire, range,
  unknown_from_serde_json,
};
use serde_json::{Value, json};

/// Document with `$schema` attached.
fn doc(body: Value) -> Value {
  let mut m = body.as_object().cloned().expect("object body");
  m.insert("$schema".into(), json!(DIALECT));
  Value::Object(m)
}

#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
struct Flat {
  #[schema("user_name")]
  name: String,
  active: bool,
  count: i64,
  small: i32,
  ratio: f64,
  nick: Option<String>,
  limit: Option<i32>,
  tags: Vec<String>,
  scores: Vec<i32>,
}

#[test]
fn derived_struct_exports_fields_required_and_nullable() {
  assert_eq!(
    json_schema_for::<Flat>(),
    doc(json!({
      "type": "object",
      "properties": {
        "user_name": {"type": "string"},
        "active": {"type": "boolean"},
        "count": {"type": "integer"},
        "small": {"type": "integer", "minimum": -2_147_483_648_i64, "maximum": 2_147_483_647},
        "ratio": {"type": "number"},
        "nick": {"anyOf": [{"type": "string"}, {"type": "null"}]},
        "limit": {
          "anyOf": [
            {"type": "integer", "minimum": -2_147_483_648_i64, "maximum": 2_147_483_647},
            {"type": "null"}
          ]
        },
        "tags": {"type": "array", "items": {"type": "string"}},
        "scores": {
          "type": "array",
          "items": {"type": "integer", "minimum": -2_147_483_648_i64, "maximum": 2_147_483_647}
        }
      },
      "required": ["user_name", "active", "count", "small", "ratio", "tags", "scores"]
    }))
  );
}

/// The exported `required` list and nullability match what the derived decoder accepts.
#[test]
fn derived_export_matches_decoding() {
  let schema = Flat::schema();
  let exported = schema.json_schema();
  let required: Vec<&str> = exported["required"]
    .as_array()
    .unwrap()
    .iter()
    .map(|v| v.as_str().unwrap())
    .collect();

  let full = json!({
    "user_name": "ada", "active": true, "count": 1, "small": 2, "ratio": 0.5,
    "nick": null, "limit": 3, "tags": [], "scores": [1]
  });
  assert!(
    schema
      .decode_unknown(&unknown_from_serde_json(full.clone()))
      .is_ok()
  );

  for key in full.as_object().unwrap().keys() {
    let mut partial = full.clone();
    partial.as_object_mut().unwrap().remove(key);
    let decoded = schema.decode_unknown(&unknown_from_serde_json(partial));
    assert_eq!(
      decoded.is_err(),
      required.contains(&key.as_str()),
      "key {key}: decode result disagrees with exported `required`"
    );
  }

  let mut overflow = full;
  overflow["small"] = json!(i64::from(i32::MAX) + 1);
  assert!(
    schema
      .decode_unknown(&unknown_from_serde_json(overflow))
      .is_err()
  );
}

#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
struct Address {
  street: String,
  zip: Option<String>,
}

#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
struct Person {
  name: String,
  home: Address,
  work: Option<Address>,
  previous: Vec<Address>,
}

#[test]
fn nested_derived_struct_is_a_ref_into_defs() {
  let address_ref = json!({"$ref": "#/$defs/schema_derive_json_schema::Address"});
  assert_eq!(
    json_schema_for::<Person>(),
    doc(json!({
      "type": "object",
      "properties": {
        "name": {"type": "string"},
        "home": address_ref,
        "work": {"anyOf": [address_ref, {"type": "null"}]},
        "previous": {"type": "array", "items": address_ref}
      },
      "required": ["name", "home", "previous"],
      "$defs": {
        "schema_derive_json_schema::Address": {
          "type": "object",
          "properties": {
            "street": {"type": "string"},
            "zip": {"anyOf": [{"type": "string"}, {"type": "null"}]}
          },
          "required": ["street"]
        }
      }
    }))
  );
  // The nested type exports on its own as an inline object.
  assert_eq!(
    json_schema_for::<Address>()["properties"]["street"],
    json!({"type": "string"})
  );
}

#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
struct Tree {
  value: i64,
  children: Vec<Tree>,
}

#[test]
fn recursive_derived_struct_terminates() {
  assert_eq!(
    json_schema_for::<Tree>(),
    doc(json!({
      "type": "object",
      "properties": {
        "value": {"type": "integer"},
        "children": {"type": "array", "items": {"$ref": "#/$defs/schema_derive_json_schema::Tree"}}
      },
      "required": ["value", "children"],
      "$defs": {
        "schema_derive_json_schema::Tree": {
          "type": "object",
          "properties": {
            "value": {"type": "integer"},
            "children": {
              "type": "array",
              "items": {"$ref": "#/$defs/schema_derive_json_schema::Tree"}
            }
          },
          "required": ["value", "children"]
        }
      }
    }))
  );
}

/// A hand-written `HasSchema` field type contributes its own AST (here: a range).
#[derive(Clone, Debug, PartialEq)]
struct Port(i64);

impl HasSchema for Port {
  type A = Port;
  type I = Unknown;
  type E = ();

  fn schema() -> Schema<Port, Unknown, ()> {
    id_effect::schema::transform(
      range(i64_unknown_wire::<()>(), 1, 65_535),
      |n| Ok(Port(n)),
      |p| p.0,
    )
  }
}

#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
struct Listener {
  port: Port,
}

#[test]
fn hand_written_has_schema_field_exports_its_ast() {
  assert_eq!(
    json_schema_for::<Listener>()["$defs"]["schema_derive_json_schema::Port"],
    json!({"type": "integer", "minimum": 1, "maximum": 65_535})
  );
  let wire = Listener::schema().encode(Listener { port: Port(8080) });
  assert_eq!(
    Listener::schema().decode_unknown(&wire).unwrap(),
    Listener { port: Port(8080) }
  );
}

#[test]
fn derived_ast_is_a_struct_of_fields() {
  let ast = Address::schema().ast().clone();
  let SchemaKind::Struct(fields) = ast.kind else {
    panic!("expected a struct AST, got {:?}", ast.kind);
  };
  assert_eq!(
    fields,
    vec![
      Field::required("street", SchemaAst::new(SchemaKind::String)),
      Field::optional(
        "zip",
        SchemaAst::new(SchemaKind::Nullable(Box::new(SchemaAst::new(
          SchemaKind::String
        ))))
      ),
    ]
  );
}
