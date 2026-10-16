//! JSON Schema (draft **2020-12**) export for any [`Schema`] / [`HasSchema`] type.
//!
//! The exporter walks the schema's [`SchemaAst`] ([`Schema::ast`]) — recorded by the same
//! combinators and validators that decode — so the exported document describes exactly the wire
//! values the schema accepts:
//!
//! | Schema | JSON Schema |
//! |--------|-------------|
//! | `string` / `i64` / `f64` / `bool_` | `type` `string` / `integer` / `number` / `boolean` |
//! | `literal_string` / `literal_i64` | `const` |
//! | `array` | `type: array`, `items` |
//! | `tuple*` | `prefixItems`, `items: false`, `minItems` = `maxItems` |
//! | `struct_*` | `type: object`, `properties`, `required` |
//! | `record` | `type: object`, `additionalProperties` |
//! | `optional` / `null_or` | `anyOf: [inner, {type: null}]` |
//! | `union_` / `union_chain` | `anyOf` |
//! | `suspend_named` | `$ref` into `$defs` (recursion) |
//! | validators | `minLength`, `maxLength`, `pattern`, `format`, `enum`, `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`, `minItems`, `maxItems` |
//! | annotations | `title`, `description`, `examples`, `default`, `deprecated` |
//!
//! Opaque schemas (built with [`Schema::make`] and no [`Schema::with_ast`]) export as `{}`; opaque
//! [`filter`](crate::schema::parse::filter) predicates and
//! [`transform`](crate::schema::parse::transform) export their inner wire schema. Unnamed
//! [`suspend`](crate::schema::extra::suspend) nodes are expanded at most
//! [`MAX_UNNAMED_LAZY_DEPTH`] levels deep, then exported as `{}`.
//!
//! The primitive fragment helpers ([`type_string`], …) from 0.7 remain for hand-built documents.
//! Requires **`schema-serde`**. See [`TESTING.md`](../../../../TESTING.md).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use crate::schema::ast::{Annotations, Constraints, LazyAst, SchemaAst, SchemaKind};
use crate::schema::has_schema::HasSchema;
use crate::schema::parse::Schema;
use crate::schema::serde_bridge::serde_json_from_unknown;

/// `$schema` URI of the emitted dialect.
pub const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Nesting limit for expanding unnamed [`SchemaKind::Lazy`] nodes.
pub const MAX_UNNAMED_LAZY_DEPTH: usize = 4;

/// JSON Schema document for `schema` (`$schema`, body, and `$defs` when recursion is named).
pub fn to_json_schema<A, I, E>(schema: &Schema<A, I, E>) -> Value {
  ast_to_json_schema(schema.ast())
}

/// JSON Schema document for a [`HasSchema`] type.
pub fn json_schema_for<T: HasSchema>() -> Value {
  to_json_schema(&T::schema())
}

/// JSON Schema document for a raw [`SchemaAst`].
pub fn ast_to_json_schema(ast: &SchemaAst) -> Value {
  let mut exporter = Exporter::default();
  let body = exporter.node(ast);
  let mut doc = Map::new();
  doc.insert("$schema".to_owned(), Value::String(DIALECT.to_owned()));
  doc.extend(body);
  if !exporter.defs.is_empty() {
    doc.insert(
      "$defs".to_owned(),
      Value::Object(exporter.defs.into_iter().collect()),
    );
  }
  Value::Object(doc)
}

impl<A, I, E> Schema<A, I, E> {
  /// JSON Schema (2020-12) document for this schema — see [`to_json_schema`].
  pub fn json_schema(&self) -> Value {
    to_json_schema(self)
  }
}

#[derive(Default)]
struct Exporter {
  defs: BTreeMap<String, Value>,
  pending: BTreeSet<String>,
  unnamed_depth: usize,
}

impl Exporter {
  fn node(&mut self, ast: &SchemaAst) -> Map<String, Value> {
    let mut m = Map::new();
    match &ast.kind {
      SchemaKind::Any => {}
      SchemaKind::Boolean => set(&mut m, "type", json!("boolean")),
      SchemaKind::Integer => set(&mut m, "type", json!("integer")),
      SchemaKind::Number => set(&mut m, "type", json!("number")),
      SchemaKind::String => set(&mut m, "type", json!("string")),
      SchemaKind::Literal(u) => set(&mut m, "const", serde_json_from_unknown(u)),
      SchemaKind::Array(inner) => {
        set(&mut m, "type", json!("array"));
        let items = Value::Object(self.node(inner));
        set(&mut m, "items", items);
      }
      SchemaKind::Tuple(items) => {
        set(&mut m, "type", json!("array"));
        let prefix: Vec<Value> = items.iter().map(|i| Value::Object(self.node(i))).collect();
        set(&mut m, "prefixItems", Value::Array(prefix));
        set(&mut m, "items", Value::Bool(false));
        set(&mut m, "minItems", json!(items.len()));
        set(&mut m, "maxItems", json!(items.len()));
      }
      SchemaKind::Struct(fields) => {
        set(&mut m, "type", json!("object"));
        let mut props = Map::new();
        for f in fields {
          props.insert(f.name.clone(), Value::Object(self.node(&f.schema)));
        }
        set(&mut m, "properties", Value::Object(props));
        let required: Vec<Value> = fields
          .iter()
          .filter(|f| f.required)
          .map(|f| Value::String(f.name.clone()))
          .collect();
        if !required.is_empty() {
          set(&mut m, "required", Value::Array(required));
        }
      }
      SchemaKind::Record(value) => {
        set(&mut m, "type", json!("object"));
        let additional = Value::Object(self.node(value));
        set(&mut m, "additionalProperties", additional);
      }
      SchemaKind::Nullable(inner) => {
        let inner = Value::Object(self.node(inner));
        set(&mut m, "anyOf", json!([inner, { "type": "null" }]));
      }
      SchemaKind::Union(arms) => {
        let arms: Vec<Value> = arms.iter().map(|a| Value::Object(self.node(a))).collect();
        set(&mut m, "anyOf", Value::Array(arms));
      }
      SchemaKind::Lazy(lazy) => self.lazy(lazy, &mut m),
    }
    constraints(&ast.constraints, &mut m);
    annotations(&ast.annotations, &mut m);
    m
  }

  fn lazy(&mut self, lazy: &LazyAst, m: &mut Map<String, Value>) {
    match lazy.name() {
      Some(name) => {
        if !self.defs.contains_key(name) && !self.pending.contains(name) {
          self.pending.insert(name.to_owned());
          let body = Value::Object(self.node(&lazy.force()));
          self.pending.remove(name);
          self.defs.insert(name.to_owned(), body);
        }
        let pointer = name.replace('~', "~0").replace('/', "~1");
        set(m, "$ref", Value::String(format!("#/$defs/{pointer}")));
      }
      None if self.unnamed_depth < MAX_UNNAMED_LAZY_DEPTH => {
        self.unnamed_depth += 1;
        let body = self.node(&lazy.force());
        self.unnamed_depth -= 1;
        m.extend(body);
      }
      None => {}
    }
  }
}

fn set(m: &mut Map<String, Value>, key: &str, value: Value) {
  m.insert(key.to_owned(), value);
}

/// JSON number for a bound: integral values print without a fraction (`5`, not `5.0`).
fn number(x: f64) -> Value {
  const EXACT: f64 = 9_007_199_254_740_992.0; // 2^53
  if x.is_finite() && x.trunc() == x && x.abs() <= EXACT {
    #[allow(clippy::cast_possible_truncation)] // integral and within ±2^53
    let n = x as i64;
    Value::from(n)
  } else {
    serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)
  }
}

fn constraints(c: &Constraints, m: &mut Map<String, Value>) {
  if let Some(n) = c.min_length {
    set(m, "minLength", json!(n));
  }
  if let Some(n) = c.max_length {
    set(m, "maxLength", json!(n));
  }
  if let Some(p) = &c.pattern {
    set(m, "pattern", json!(p));
  }
  if let Some(f) = &c.format {
    set(m, "format", json!(f));
  }
  if let Some(x) = c.minimum {
    set(m, "minimum", number(x));
  }
  if let Some(x) = c.maximum {
    set(m, "maximum", number(x));
  }
  if let Some(x) = c.exclusive_minimum {
    set(m, "exclusiveMinimum", number(x));
  }
  if let Some(x) = c.exclusive_maximum {
    set(m, "exclusiveMaximum", number(x));
  }
  if let Some(n) = c.min_items {
    set(m, "minItems", json!(n));
  }
  if let Some(n) = c.max_items {
    set(m, "maxItems", json!(n));
  }
  if let Some(values) = &c.enum_values {
    let values = values.iter().map(serde_json_from_unknown).collect();
    set(m, "enum", Value::Array(values));
  }
}

fn annotations(a: &Annotations, m: &mut Map<String, Value>) {
  if let Some(t) = &a.title {
    set(m, "title", json!(t));
  }
  if let Some(d) = &a.description {
    set(m, "description", json!(d));
  }
  if !a.examples.is_empty() {
    let examples = a.examples.iter().map(serde_json_from_unknown).collect();
    set(m, "examples", Value::Array(examples));
  }
  if let Some(d) = &a.default {
    set(m, "default", serde_json_from_unknown(d));
  }
  if a.deprecated {
    set(m, "deprecated", Value::Bool(true));
  }
}

/// `"type": "string"`.
pub fn type_string() -> Value {
  json!({ "type": "string" })
}

/// `"type": "integer"` without `format`.
pub fn type_integer() -> Value {
  json!({ "type": "integer" })
}

/// `"type": "number"`.
pub fn type_number() -> Value {
  json!({ "type": "number" })
}

/// `"type": "boolean"`.
pub fn type_boolean() -> Value {
  json!({ "type": "boolean" })
}

/// `{ "type": "array", "items": items }`.
pub fn type_array(items: Value) -> Value {
  json!({ "type": "array", "items": items })
}

/// `{ "type": "object", "additionalProperties": value_schema }` for string-keyed records.
pub fn type_record(value_schema: Value) -> Value {
  json!({
    "type": "object",
    "additionalProperties": value_schema
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::schema::parse::{Unknown, i64, string};

  #[test]
  fn primitive_fragments_agree_with_schema_export() {
    let body = |v: Value| {
      let mut m = v.as_object().cloned().unwrap_or_default();
      m.remove("$schema");
      Value::Object(m)
    };
    assert_eq!(body(string::<()>().json_schema()), type_string());
    assert_eq!(body(i64::<()>().json_schema()), type_integer());
  }

  #[test]
  fn opaque_schema_exports_empty_object() {
    let s = Schema::<i64, i64, ()>::make(Ok, |n| n, |_| Ok(0));
    assert_eq!(s.json_schema(), json!({ "$schema": DIALECT }));
  }

  #[test]
  fn bounds_print_integral_values_without_fraction() {
    assert_eq!(number(5.0), json!(5));
    assert_eq!(number(-0.5), json!(-0.5));
    assert_eq!(number(f64::INFINITY), Value::Null);
  }

  #[test]
  fn ref_names_are_json_pointer_escaped() {
    let ast = SchemaAst::new(SchemaKind::Lazy(LazyAst::new(Some("a/b~c".into()), || {
      SchemaAst::new(SchemaKind::Literal(Unknown::Null))
    })));
    let doc = ast_to_json_schema(&ast);
    assert_eq!(doc["$ref"], json!("#/$defs/a~1b~0c"));
    assert_eq!(doc["$defs"]["a/b~c"], json!({ "const": null }));
  }

  #[test]
  fn string_fragment_is_stable() {
    assert_eq!(type_string(), json!({"type": "string"}));
  }

  #[test]
  fn primitive_fragments_match_draft_style() {
    assert_eq!(type_integer(), json!({"type": "integer"}));
    assert_eq!(type_number(), json!({"type": "number"}));
    assert_eq!(type_boolean(), json!({"type": "boolean"}));
  }

  #[test]
  fn array_and_record_wrap_items() {
    let items = type_string();
    assert_eq!(
      type_array(items.clone()),
      json!({ "type": "array", "items": items })
    );
    assert_eq!(
      type_record(type_boolean()),
      json!({
        "type": "object",
        "additionalProperties": type_boolean()
      })
    );
  }
}
