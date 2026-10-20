//! [`RouteInfo`] → [`Operation`]: parameters, request body, responses and the component schemas
//! they refer to.

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::http::{Method, StatusCode};
use id_effect::schema::json_schema_export::DIALECT;
use serde_json::{Map, Value, json};

use super::model::{
  Components, MediaType, Operation, Parameter, ParameterIn, PathItem, RequestBody, Response,
};
use crate::route::{BodyKind, PROBLEM_JSON, RouteAccess, RouteInfo, SchemaRef};

/// Media type of documented (`Route::response`) bodies.
const JSON: &str = "application/json";

/// What the routes of one document produce: paths and component schemas.
pub(super) struct Built {
  pub(super) paths: BTreeMap<String, PathItem>,
  pub(super) components: Components,
}

/// Paths and component schemas for `routes`, in registration order.
pub(super) fn build<'a>(routes: impl IntoIterator<Item = &'a RouteInfo>) -> Built {
  let mut generator = Generator::default();
  let mut paths: BTreeMap<String, PathItem> = BTreeMap::new();
  for info in routes {
    let (path, _) = parse_template(&info.path);
    let item = paths.entry(path).or_default();
    if let Some(slot) = item.slot(&info.method) {
      *slot = Some(generator.operation(info));
    }
  }
  paths.retain(|_, item| *item != PathItem::default());
  Built {
    paths,
    components: Components {
      schemas: generator.schemas,
      ..Components::default()
    },
  }
}

/// Collects component schemas (one per declaring type, `$ref`-deduplicated) and operation ids.
#[derive(Default)]
struct Generator {
  schemas: BTreeMap<String, Value>,
  /// Declaring type name → component name.
  by_type: HashMap<&'static str, String>,
  /// Hoisted `$defs`: (definition name, exported body, component name).
  defs: Vec<(String, Value, String)>,
  problem: Option<String>,
  operation_ids: HashSet<String>,
}

impl Generator {
  fn operation(&mut self, info: &RouteInfo) -> Operation {
    let mut op = Operation {
      operation_id: self.operation_id(&info.method, &info.path),
      tags: tag(&info.path).into_iter().collect(),
      policies: info.policies.clone(),
      ..Operation::default()
    };
    op.parameters = self.path_parameters(&info.path, info.path_params.as_ref());
    if let Some(query) = &info.query {
      let params = self.object_parameters(query, ParameterIn::Query);
      op.parameters.extend(params);
    }
    if let Some(headers) = &info.headers {
      let params = self.object_parameters(headers, ParameterIn::Header);
      op.parameters.extend(params);
    }
    if let Some(body) = &info.body {
      let schema = self.component(body);
      op.request_body = Some(RequestBody {
        description: None,
        content: info
          .body_kind
          .media_types()
          .iter()
          .map(|media| ((*media).to_owned(), MediaType::new(schema.clone())))
          .collect(),
        required: true,
      });
    }

    let mut documented: BTreeMap<StatusCode, Vec<Value>> = BTreeMap::new();
    for response in &info.responses {
      let schema = self.component(&response.schema);
      let schemas = documented.entry(response.status).or_default();
      if !schemas.contains(&schema) {
        schemas.push(schema);
      }
    }
    for (status, mut schemas) in documented {
      let schema = if schemas.len() == 1 {
        schemas.remove(0)
      } else {
        json!({ "anyOf": schemas })
      };
      op.responses.insert(
        status.as_str().to_owned(),
        Response::new(reason(status)).with_content(JSON, schema),
      );
    }

    let declares_input = info.path_params.is_some()
      || info.query.is_some()
      || info.headers.is_some()
      || info.body.is_some();
    if declares_input {
      self.problem_response(&mut op, StatusCode::UNPROCESSABLE_ENTITY);
    }
    if info.body.is_some() && info.body_kind == BodyKind::Json {
      self.problem_response(&mut op, StatusCode::BAD_REQUEST);
    }
    if !info.policies.is_empty() {
      self.problem_response(&mut op, StatusCode::UNAUTHORIZED);
      self.problem_response(&mut op, StatusCode::FORBIDDEN);
    }
    if info.access == RouteAccess::Public {
      // Declared public: an empty requirement list, "no authentication" (OpenAPI 3.1 §4.8.10).
      op.security = Some(Vec::new());
    }
    if op.responses.is_empty() {
      op.responses
        .insert("default".to_owned(), Response::new("Response"));
    }
    op
  }

  /// `method_segment_by_param` (`GET /accounts/{id}` → `get_accounts_by_id`), unique in the
  /// document.
  fn operation_id(&mut self, method: &Method, template: &str) -> String {
    let mut id = ident(method.as_str());
    let mut segments = template.split('/').filter(|s| !s.is_empty()).peekable();
    if segments.peek().is_none() {
      id.push_str("_root");
    }
    for segment in segments {
      id.push('_');
      match param_name(segment) {
        Some(name) => {
          id.push_str("by_");
          id.push_str(&ident(name));
        }
        None => id.push_str(&ident(segment)),
      }
    }
    let id = unique(&id, |candidate| self.operation_ids.contains(candidate));
    self.operation_ids.insert(id.clone());
    id
  }

  /// One required parameter per `{name}` of the template, typed from the declared path schema:
  /// the struct property of that name, the tuple item at that position, or the whole schema for
  /// a single parameter; `string` when undeclared.
  fn path_parameters(&mut self, template: &str, schema: Option<&SchemaRef>) -> Vec<Parameter> {
    let (_, names) = parse_template(template);
    let doc = schema.map(|s| self.inline(s));
    names
      .iter()
      .enumerate()
      .map(|(index, name)| {
        let schema = doc
          .as_ref()
          .and_then(|doc| path_param_schema(doc, name, index, names.len()))
          .unwrap_or_else(|| json!({ "type": "string" }));
        Parameter::new(name.clone(), ParameterIn::Path, schema)
      })
      .collect()
  }

  /// One parameter per property of a struct schema (required unless the extractor fills it in
  /// when absent: arrays and nullable fields); a non-struct query schema is one free-form
  /// `style: form` object parameter.
  fn object_parameters(&mut self, schema: &SchemaRef, location: ParameterIn) -> Vec<Parameter> {
    let doc = self.inline(schema);
    if let Some(properties) = doc.get("properties").and_then(Value::as_object) {
      let required: Vec<&str> = doc
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
      return properties
        .iter()
        .map(|(name, property)| {
          let required = required.contains(&name.as_str()) && !filled_when_absent(property);
          Parameter::new(name.clone(), location, property.clone()).with_required(required)
        })
        .collect();
    }
    if location != ParameterIn::Query {
      return Vec::new();
    }
    let mut param = Parameter::new(component_name(schema.short_name()), location, doc);
    param.style = Some("form".to_owned());
    param.explode = Some(true);
    vec![param]
  }

  /// `{"$ref": "#/components/schemas/<Name>"}` for `schema`, registering the component (the
  /// exported JSON Schema document, `$defs` hoisted) the first time its type is seen.
  fn component(&mut self, schema: &SchemaRef) -> Value {
    if let Some(name) = self.by_type.get(schema.type_name()) {
      return schema_ref(name);
    }
    let name = self.reserve(&component_name(schema.short_name()));
    self.by_type.insert(schema.type_name(), name.clone());
    let doc = self.hoist(schema.json_schema());
    self.schemas.insert(name.clone(), doc);
    schema_ref(&name)
  }

  /// The exported schema without `$schema`, for use inside a parameter.
  fn inline(&mut self, schema: &SchemaRef) -> Value {
    let mut doc = self.hoist(schema.json_schema());
    if let Some(map) = doc.as_object_mut() {
      map.remove("$schema");
    }
    doc
  }

  /// Move the `$defs` of an exported document (named recursive schemas) into the components and
  /// point its `$ref`s there: inside an OpenAPI document `#` is the document, not the schema.
  fn hoist(&mut self, mut doc: Value) -> Value {
    let Some(Value::Object(defs)) = doc.as_object_mut().and_then(|m| m.remove("$defs")) else {
      return doc;
    };
    let mut names = HashMap::new();
    let mut fresh = Vec::new();
    for (key, body) in defs {
      let token = key.replace('~', "~0").replace('/', "~1");
      let seen = self
        .defs
        .iter()
        .find(|(k, b, _)| *k == key && *b == body)
        .map(|(_, _, name)| name.clone());
      let name = match seen {
        Some(name) => name,
        None => {
          let name = self.reserve(&component_name(&key));
          self.defs.push((key, body.clone(), name.clone()));
          fresh.push((name.clone(), body));
          name
        }
      };
      names.insert(token, name);
    }
    for (name, mut body) in fresh {
      rewrite_refs(&mut body, &names);
      self.schemas.insert(name, body);
    }
    rewrite_refs(&mut doc, &names);
    doc
  }

  /// A component name derived from `base` that is not taken yet, reserved.
  fn reserve(&mut self, base: &str) -> String {
    let name = unique(base, |candidate| self.schemas.contains_key(candidate));
    self.schemas.insert(name.clone(), Value::Null);
    name
  }

  /// Document `status` as a [`Problem`](crate::Problem) response unless the route already does.
  fn problem_response(&mut self, op: &mut Operation, status: StatusCode) {
    let key = status.as_str().to_owned();
    if op.responses.contains_key(&key) {
      return;
    }
    let name = match &self.problem {
      Some(name) => name.clone(),
      None => {
        let name = self.reserve("Problem");
        self.schemas.insert(name.clone(), problem_schema());
        self.problem = Some(name.clone());
        name
      }
    };
    let response = Response::new(reason(status)).with_content(PROBLEM_JSON, schema_ref(&name));
    op.responses.insert(key, response);
  }
}

/// JSON Schema of an RFC 9457 [`Problem`](crate::Problem) body as the typed routes send it.
pub(super) fn problem_schema() -> Value {
  json!({
    "$schema": DIALECT,
    "title": "Problem",
    "description": "RFC 9457 problem details; `errors` lists every input issue.",
    "type": "object",
    "properties": {
      "type": { "type": "string", "format": "uri-reference", "default": "about:blank" },
      "title": { "type": "string" },
      "status": { "type": "integer", "minimum": 100, "maximum": 599 },
      "detail": { "type": "string" },
      "instance": { "type": "string", "format": "uri-reference" },
      "errors": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "in": { "enum": ["path", "query", "header", "body"] },
            "pointer": { "type": "string" },
            "message": { "type": "string" }
          },
          "required": ["in", "pointer", "message"]
        }
      }
    },
    "required": ["type", "title", "status"]
  })
}

fn schema_ref(name: &str) -> Value {
  json!({ "$ref": format!("#/components/schemas/{name}") })
}

/// The status' reason phrase (`Response` for an unregistered code).
fn reason(status: StatusCode) -> String {
  status.canonical_reason().unwrap_or("Response").to_owned()
}

/// `base`, or `base_2`, `base_3`, … — the first for which `taken` is false.
fn unique(base: &str, taken: impl Fn(&str) -> bool) -> String {
  if !taken(base) {
    return base.to_owned();
  }
  (2..)
    .map(|n| format!("{base}_{n}"))
    .find(|candidate| !taken(candidate))
    .unwrap_or_else(|| base.to_owned())
}

/// The OpenAPI path of an Axum template (`{*rest}` → `{rest}`) and its parameter names in order.
/// `{{` / `}}` are literal braces.
pub(super) fn parse_template(template: &str) -> (String, Vec<String>) {
  let mut path = String::with_capacity(template.len());
  let mut names = Vec::new();
  let mut rest = template;
  while let Some(open) = rest.find('{') {
    path.push_str(&rest[..open]);
    let after = &rest[open + 1..];
    if let Some(literal) = after.strip_prefix('{') {
      path.push_str("{{");
      rest = literal;
      continue;
    }
    let Some(close) = after.find('}') else {
      path.push_str(&rest[open..]);
      return (path, names);
    };
    let name = after[..close].trim_start_matches('*');
    path.push('{');
    path.push_str(name);
    path.push('}');
    names.push(name.to_owned());
    rest = &after[close + 1..];
  }
  path.push_str(rest);
  (path, names)
}

/// `name` for a whole-segment parameter `{name}` / `{*name}`.
fn param_name(segment: &str) -> Option<&str> {
  segment
    .strip_prefix('{')
    .and_then(|s| s.strip_suffix('}'))
    .map(|s| s.trim_start_matches('*'))
}

/// The tag of a path: its first literal segment.
fn tag(template: &str) -> Option<String> {
  template
    .split('/')
    .find(|s| !s.is_empty())
    .filter(|s| !s.starts_with('{'))
    .map(str::to_owned)
}

/// Lower-case ASCII identifier: other characters become `_`.
fn ident(s: &str) -> String {
  s.chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() {
        c.to_ascii_lowercase()
      } else {
        '_'
      }
    })
    .collect()
}

/// A component name (`^[a-zA-Z0-9._-]+$`) for a Rust type name: module paths are dropped and
/// generic punctuation becomes `_` (`Page<my::Item>` → `Page_Item`).
pub(super) fn component_name(raw: &str) -> String {
  let mut out = String::new();
  let mut segment = String::new();
  for c in raw.chars() {
    if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
      segment.push(c);
    } else if c == ':' {
      segment.clear();
    } else {
      out.push_str(&segment);
      segment.clear();
      if !out.is_empty() && !out.ends_with('_') {
        out.push('_');
      }
    }
  }
  out.push_str(&segment);
  let trimmed = out.trim_end_matches('_');
  if trimmed.is_empty() {
    "Schema".to_owned()
  } else {
    trimmed.to_owned()
  }
}

/// The schema of path parameter `name` (at `index` of `count`) within a declared path schema.
fn path_param_schema(doc: &Value, name: &str, index: usize, count: usize) -> Option<Value> {
  if let Some(properties) = doc.get("properties") {
    return properties.get(name).cloned();
  }
  if let Some(items) = doc.get("prefixItems").and_then(Value::as_array) {
    return (items.len() == count).then(|| items[index].clone());
  }
  (count == 1).then(|| doc.clone())
}

/// Whether the query/header extractor fills in an absent required field (`[]` for an array,
/// `null` for a nullable field).
fn filled_when_absent(property: &Value) -> bool {
  let is_null = |arm: &Value| arm.get("type").and_then(Value::as_str) == Some("null");
  property.get("type").and_then(Value::as_str) == Some("array")
    || property
      .get("anyOf")
      .and_then(Value::as_array)
      .is_some_and(|arms| arms.iter().any(is_null))
}

/// Point `#/$defs/<token>` references at the hoisted components; literal values (`const`,
/// `enum`, `examples`, `default`) are left alone.
fn rewrite_refs(value: &mut Value, names: &HashMap<String, String>) {
  match value {
    Value::Object(map) => {
      rewrite_ref(map, names);
      for (key, child) in map.iter_mut() {
        if !matches!(key.as_str(), "const" | "enum" | "examples" | "default") {
          rewrite_refs(child, names);
        }
      }
    }
    Value::Array(items) => {
      for item in items {
        rewrite_refs(item, names);
      }
    }
    _ => {}
  }
}

fn rewrite_ref(map: &mut Map<String, Value>, names: &HashMap<String, String>) {
  let target = map
    .get("$ref")
    .and_then(Value::as_str)
    .and_then(|r| r.strip_prefix("#/$defs/"))
    .and_then(|token| names.get(token));
  if let Some(name) = target {
    let pointer = format!("#/components/schemas/{name}");
    map.insert("$ref".to_owned(), Value::String(pointer));
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn templates_become_openapi_paths() {
    assert_eq!(
      parse_template("/files/{*rest}"),
      ("/files/{rest}".to_owned(), vec!["rest".to_owned()])
    );
    assert_eq!(
      parse_template("/a/{x}/b/{y}"),
      (
        "/a/{x}/b/{y}".to_owned(),
        vec!["x".to_owned(), "y".to_owned()]
      )
    );
    assert_eq!(
      parse_template("/lit/{{x}}"),
      ("/lit/{{x}}".to_owned(), vec![])
    );
    assert_eq!(parse_template("/open/{x"), ("/open/{x".to_owned(), vec![]));
  }

  #[test]
  fn component_names_drop_module_paths() {
    assert_eq!(component_name("Page<my::Item>"), "Page_Item");
    assert_eq!(component_name("(i64, alloc::string::String)"), "i64_String");
    assert_eq!(component_name("&str"), "str");
    assert_eq!(component_name("<>"), "Schema");
    assert_eq!(component_name("a/b~c"), "a_b_c");
  }

  #[test]
  fn names_are_made_unique() {
    assert_eq!(unique("a", |_| false), "a");
    assert_eq!(unique("a", |c| c == "a" || c == "a_2"), "a_3");
    assert_eq!(reason(StatusCode::from_u16(599).unwrap()), "Response");
    assert_eq!(tag("/{id}/x"), None);
    assert_eq!(tag("/"), None);
    assert_eq!(param_name("{*rest}"), Some("rest"));
  }

  #[test]
  fn path_parameters_follow_struct_tuple_and_scalar_schemas() {
    let tuple = json!({ "prefixItems": [{ "type": "string" }, { "type": "integer" }] });
    assert_eq!(
      path_param_schema(&tuple, "b", 1, 2),
      Some(json!({ "type": "integer" }))
    );
    assert_eq!(path_param_schema(&tuple, "c", 0, 3), None);
    let scalar = json!({ "type": "integer" });
    assert_eq!(path_param_schema(&scalar, "id", 0, 1), Some(scalar.clone()));
    assert_eq!(path_param_schema(&scalar, "id", 0, 2), None);
    let object = json!({ "properties": { "id": { "type": "integer" } } });
    assert_eq!(path_param_schema(&object, "other", 0, 1), None);
  }

  #[test]
  fn literal_values_keep_their_ref_lookalikes() {
    let names = HashMap::from([("T".to_owned(), "T".to_owned())]);
    let mut value = json!({
      "const": { "$ref": "#/$defs/T" },
      "items": [{ "$ref": "#/$defs/T" }, { "$ref": "#/$defs/Other" }, 1]
    });
    rewrite_refs(&mut value, &names);
    assert_eq!(value["const"]["$ref"], "#/$defs/T");
    assert_eq!(value["items"][0]["$ref"], "#/components/schemas/T");
    assert_eq!(value["items"][1]["$ref"], "#/$defs/Other");
  }
}
