//! Golden tests: JSON Schema (2020-12) export, one per schema kind (data.jsonschema#1, #4, #5, #6).
//!
//! No JSON Schema validator crate is in the dependency tree, so these are golden only; the
//! "same schema" tests check that the value used for export is the value that validates.

#![cfg(feature = "schema-serde")]

use id_effect::schema::json_schema_export::{DIALECT, json_schema_for};
use id_effect::schema::{
  HasSchema, Schema, Unknown, array, bool_, email, exclusive_maximum, exclusive_minimum, f64, i64,
  i64_unknown_wire, length, literal_i64, literal_string, max_items, min_items, null_or, one_of,
  optional, pattern, range, record, refine, string, struct_, struct3, struct5, suspend,
  suspend_named, to_json_schema, transform, tuple, tuple5, union_, union_chain,
  unknown_from_serde_json, url, uuid,
};
use serde_json::{Value, json};

/// Document with `$schema` attached.
fn doc(body: Value) -> Value {
  let mut m = body.as_object().cloned().expect("object body");
  m.insert("$schema".into(), json!(DIALECT));
  Value::Object(m)
}

#[test]
fn primitives() {
  assert_eq!(string::<()>().json_schema(), doc(json!({"type": "string"})));
  assert_eq!(i64::<()>().json_schema(), doc(json!({"type": "integer"})));
  assert_eq!(
    i64_unknown_wire::<()>().json_schema(),
    doc(json!({"type": "integer"}))
  );
  assert_eq!(f64::<()>().json_schema(), doc(json!({"type": "number"})));
  assert_eq!(bool_::<()>().json_schema(), doc(json!({"type": "boolean"})));
}

#[test]
fn literals() {
  assert_eq!(
    literal_string::<()>("on").json_schema(),
    doc(json!({"const": "on"}))
  );
  assert_eq!(literal_i64::<()>(7).json_schema(), doc(json!({"const": 7})));
}

#[test]
fn array_and_record() {
  assert_eq!(
    array(string::<()>()).json_schema(),
    doc(json!({"type": "array", "items": {"type": "string"}}))
  );
  assert_eq!(
    record(i64::<()>()).json_schema(),
    doc(json!({"type": "object", "additionalProperties": {"type": "integer"}}))
  );
}

#[test]
fn tuples() {
  assert_eq!(
    tuple(i64::<()>(), string::<()>()).json_schema(),
    doc(json!({
      "type": "array",
      "prefixItems": [{"type": "integer"}, {"type": "string"}],
      "items": false,
      "minItems": 2,
      "maxItems": 2
    }))
  );
  let t5 = tuple5(i64(), i64(), i64(), i64(), bool_::<()>());
  assert_eq!(
    t5.json_schema()["prefixItems"][4],
    json!({"type": "boolean"})
  );
  assert_eq!(t5.json_schema()["maxItems"], json!(5));
}

#[test]
fn structs() {
  assert_eq!(
    struct_("name", string::<()>(), "age", i64::<()>()).json_schema(),
    doc(json!({
      "type": "object",
      "properties": {"name": {"type": "string"}, "age": {"type": "integer"}},
      "required": ["name", "age"]
    }))
  );
  let wide = struct5(
    "a",
    i64(),
    "b",
    string(),
    "c",
    bool_(),
    "d",
    f64(),
    "e",
    i64::<()>(),
  );
  assert_eq!(
    wide.json_schema()["required"],
    json!(["a", "b", "c", "d", "e"])
  );
  assert_eq!(
    wide.json_schema()["properties"]["d"],
    json!({"type": "number"})
  );
}

#[test]
fn optional_and_unions() {
  let nullable = json!({"anyOf": [{"type": "integer"}, {"type": "null"}]});
  assert_eq!(optional(i64::<()>()).json_schema(), doc(nullable.clone()));
  assert_eq!(null_or(i64::<()>()).json_schema(), doc(nullable));
  assert_eq!(
    union_(i64_unknown_wire::<()>(), i64_unknown_wire::<()>()).json_schema(),
    doc(json!({"anyOf": [{"type": "integer"}, {"type": "integer"}]}))
  );
  // Nested un-annotated unions flatten into one `anyOf`.
  let nested = union_chain(vec![
    union_(i64_unknown_wire::<()>(), i64_unknown_wire::<()>()),
    i64_unknown_wire::<()>(),
  ]);
  assert_eq!(
    nested.json_schema()["anyOf"].as_array().map(Vec::len),
    Some(3)
  );
}

#[test]
fn transform_and_opaque_refine_export_the_wire_schema() {
  let port = transform(
    range(i64::<()>(), 1, 65_535),
    |n| u16::try_from(n).map_err(|_| id_effect::schema::ParseError::new("", "port")),
    i64::from,
  );
  assert_eq!(
    port.json_schema(),
    doc(json!({"type": "integer", "minimum": 1, "maximum": 65535}))
  );
  let even = refine(i64::<()>(), |n| n % 2 == 0, "even");
  assert_eq!(even.json_schema(), doc(json!({"type": "integer"})));
}

#[test]
fn constraints_are_reflected() {
  let pat = pattern(string::<()>(), "^[a-z]+$", |s| {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase())
  });
  assert_eq!(
    struct5(
      "name",
      length(string::<()>(), 1, 40),
      "slug",
      pat,
      "color",
      one_of(string::<()>(), &["red", "green"]),
      "age",
      range(i64::<()>(), 0, 150),
      "ratio",
      exclusive_maximum(exclusive_minimum(f64::<()>(), 0.0), 1.5),
    )
    .json_schema(),
    doc(json!({
      "type": "object",
      "properties": {
        "name": {"type": "string", "minLength": 1, "maxLength": 40},
        "slug": {"type": "string", "pattern": "^[a-z]+$"},
        "color": {"type": "string", "enum": ["red", "green"]},
        "age": {"type": "integer", "minimum": 0, "maximum": 150},
        "ratio": {"type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 1.5}
      },
      "required": ["name", "slug", "color", "age", "ratio"]
    }))
  );
  assert_eq!(
    struct3(
      "email",
      email(string::<()>()),
      "id",
      uuid(string::<()>()),
      "home",
      url(string::<()>()),
    )
    .json_schema()["properties"],
    json!({
      "email": {"type": "string", "format": "email"},
      "id": {"type": "string", "format": "uuid"},
      "home": {"type": "string", "format": "uri"}
    })
  );
  assert_eq!(
    max_items(min_items(array(i64::<()>()), 1), 10).json_schema(),
    doc(json!({"type": "array", "items": {"type": "integer"}, "minItems": 1, "maxItems": 10}))
  );
}

#[test]
fn annotations_are_emitted() {
  let user = struct_(
    "email",
    email(string::<()>())
      .describe("Login address")
      .example(Unknown::String("ada@example.com".into())),
    "nick",
    optional(string::<()>())
      .deprecated()
      .default_value(Unknown::Null),
  )
  .title("User")
  .describe("A registered user");
  assert_eq!(
    user.json_schema(),
    doc(json!({
      "title": "User",
      "description": "A registered user",
      "type": "object",
      "properties": {
        "email": {
          "type": "string",
          "format": "email",
          "description": "Login address",
          "examples": ["ada@example.com"]
        },
        "nick": {
          "anyOf": [{"type": "string"}, {"type": "null"}],
          "deprecated": true,
          "default": null
        }
      },
      "required": ["email", "nick"]
    }))
  );
}

/// `{ value: i64, children: [Node] }`, recursive via `suspend_named`.
#[derive(Clone, Debug, PartialEq)]
struct NodeTree(i64, Vec<NodeTree>);

/// Hand-written codec over an [`Unknown`] wire (recursive wire tuples are not expressible), described
/// for export with `with_ast` from the `struct_` it decodes through.
fn tree() -> Schema<NodeTree, Unknown, ()> {
  let shape = struct_(
    "value",
    i64::<()>(),
    "children",
    array(suspend_named("Node", tree)),
  );
  let (dec, enc, du) = (shape.clone(), shape.clone(), shape.clone());
  Schema::make(
    move |u: Unknown| dec.decode_unknown(&u).map(|(v, k)| NodeTree(v, k)),
    move |NodeTree(v, kids)| {
      let (v, kids) = enc.encode((v, kids));
      Unknown::Object(
        [
          ("value".to_owned(), Unknown::I64(v)),
          ("children".to_owned(), Unknown::Array(kids)),
        ]
        .into_iter()
        .collect(),
      )
    },
    move |u| du.decode_unknown(u).map(|(v, k)| NodeTree(v, k)),
  )
  .with_ast(shape.ast().clone())
  .describe("tree node")
}

#[test]
fn recursion_uses_defs_and_ref() {
  let root = suspend_named("Node", tree);
  assert_eq!(
    root.json_schema(),
    json!({
      "$schema": DIALECT,
      "$ref": "#/$defs/Node",
      "$defs": {
        "Node": {
          "type": "object",
          "description": "tree node",
          "properties": {
            "value": {"type": "integer"},
            "children": {"type": "array", "items": {"$ref": "#/$defs/Node"}}
          },
          "required": ["value", "children"]
        }
      }
    })
  );
  // The same schema value still decodes recursively.
  let input = unknown_from_serde_json(json!({
    "value": 1,
    "children": [{"value": 2, "children": []}]
  }));
  assert_eq!(
    root.decode_unknown(&input).unwrap(),
    NodeTree(1, vec![NodeTree(2, vec![])])
  );
}

#[test]
fn unnamed_suspend_expands_with_a_depth_bound() {
  let inner: Schema<Vec<i64>, Vec<i64>, ()> = suspend(|| array(i64::<()>()));
  assert_eq!(
    inner.json_schema(),
    doc(json!({"type": "array", "items": {"type": "integer"}}))
  );
}

struct Signup;

impl HasSchema for Signup {
  type A = (String, i64);
  type I = (String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I, Self::E> {
    struct_(
      "email",
      email(string::<()>()),
      "age",
      range(i64::<()>(), 13, 130),
    )
  }
}

#[test]
fn has_schema_types_export() {
  assert_eq!(
    json_schema_for::<Signup>(),
    doc(json!({
      "type": "object",
      "properties": {
        "email": {"type": "string", "format": "email"},
        "age": {"type": "integer", "minimum": 13, "maximum": 130}
      },
      "required": ["email", "age"]
    }))
  );
  assert_eq!(
    json_schema_for::<Signup>(),
    to_json_schema(&Signup::schema())
  );
}

/// data.jsonschema#4: one schema value both validates and documents.
#[test]
fn same_schema_validates_and_exports() {
  let schema = Signup::schema();
  let exported = schema.json_schema();
  assert_eq!(exported["properties"]["age"]["minimum"], json!(13));

  let bad = unknown_from_serde_json(json!({"email": "nope", "age": 7}));
  let errs = schema.decode_unknown_all(&bad).unwrap_err();
  let paths: Vec<_> = errs.issues.iter().map(|i| i.path.as_str()).collect();
  assert_eq!(paths, ["email", "age"]);

  let good = unknown_from_serde_json(json!({"email": "kid@school.edu", "age": 13}));
  assert_eq!(
    schema.decode_unknown(&good).unwrap(),
    ("kid@school.edu".to_owned(), 13)
  );
}
