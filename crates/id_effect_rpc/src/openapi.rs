//! OpenAPI 3.0 document emission from HTTP route metadata.
//!
//! Describe HTTP-shaped operations with [`RpcServiceDef`], build an [`OpenApiSpec`], then call
//! [`emit_openapi_json`] or [`emit_openapi_yaml`]. The contract-driven OpenAPI 3.1 and AsyncAPI
//! export of 0.12 (r12-e12-4-openapi-asyncapi) reads [`crate::contract::Contract`] instead.

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// HTTP verb of an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcHttpMethod {
  /// GET
  Get,
  /// POST
  Post,
  /// PUT
  Put,
  /// DELETE
  Delete,
  /// PATCH
  Patch,
}

impl RpcHttpMethod {
  /// Uppercase method name (`GET`, `POST`, …).
  #[inline]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Get => "GET",
      Self::Post => "POST",
      Self::Put => "PUT",
      Self::Delete => "DELETE",
      Self::Patch => "PATCH",
    }
  }
}

/// Metadata for one operation (one route + handler).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcMethodDef {
  /// Stable operation id (OpenAPI `operationId`).
  pub operation: String,
  /// Route path (e.g. `/greet`).
  pub path: String,
  /// HTTP method.
  pub method: RpcHttpMethod,
  /// Optional short description.
  pub summary: Option<String>,
  /// Request payload type name (when the route accepts a body).
  pub request_type: Option<String>,
  /// Success response type name.
  pub response_type: Option<String>,
}

/// Operations exposed as one logical service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcServiceDef {
  /// Service name (PascalCase recommended).
  pub service: String,
  /// Operations belonging to this service.
  pub methods: Vec<RpcMethodDef>,
}

/// Top-level OpenAPI info block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenApiInfo {
  /// API title.
  pub title: String,
  /// Semver or date string.
  pub version: String,
}

/// OpenAPI document inputs (service routes + info).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenApiSpec {
  /// Document metadata.
  pub info: OpenApiInfo,
  /// RPC operations to expose as HTTP paths.
  pub service: RpcServiceDef,
}

/// Serialize an OpenAPI 3.0.3 document as pretty JSON.
pub fn emit_openapi_json(spec: &OpenApiSpec) -> Result<String, serde_json::Error> {
  let value = build_openapi_value(spec);
  serde_json::to_string_pretty(&value)
}

/// Serialize an OpenAPI 3.0.3 document as YAML.
pub fn emit_openapi_yaml(spec: &OpenApiSpec) -> Result<String, serde_yaml::Error> {
  let value = build_openapi_value(spec);
  serde_yaml::to_string(&value)
}

fn build_openapi_value(spec: &OpenApiSpec) -> Value {
  let mut paths = BTreeMap::new();
  let mut schemas = BTreeMap::new();

  for method in &spec.service.methods {
    let path_item = paths
      .entry(method.path.clone())
      .or_insert_with(|| json!({}));
    let http_key = http_method_key(method.method);
    let operation = build_operation(method, &mut schemas);
    if let Some(obj) = path_item.as_object_mut() {
      obj.insert(http_key.to_owned(), operation);
    }
  }

  let components = if schemas.is_empty() {
    json!({})
  } else {
    json!({ "schemas": schemas })
  };

  json!({
    "openapi": "3.0.3",
    "info": {
      "title": spec.info.title,
      "version": spec.info.version,
    },
    "paths": paths,
    "components": components,
  })
}

fn http_method_key(method: RpcHttpMethod) -> &'static str {
  match method {
    RpcHttpMethod::Get => "get",
    RpcHttpMethod::Post => "post",
    RpcHttpMethod::Put => "put",
    RpcHttpMethod::Delete => "delete",
    RpcHttpMethod::Patch => "patch",
  }
}

fn build_operation(method: &RpcMethodDef, schemas: &mut BTreeMap<String, Value>) -> Value {
  let mut op = json!({
    "operationId": method.operation,
    "responses": {
      "200": {
        "description": "Success",
        "content": {
          "application/json": {
            "schema": response_schema(method, schemas),
          }
        }
      }
    }
  });

  if let Some(summary) = &method.summary
    && let Some(obj) = op.as_object_mut()
  {
    obj.insert("summary".to_owned(), json!(summary));
  }

  if method.request_type.is_some()
    && let Some(obj) = op.as_object_mut()
  {
    obj.insert(
      "requestBody".to_owned(),
      json!({
        "required": true,
        "content": {
          "application/json": {
            "schema": request_schema(method, schemas),
          }
        }
      }),
    );
  }

  op
}

fn request_schema(method: &RpcMethodDef, schemas: &mut BTreeMap<String, Value>) -> Value {
  schema_ref(method.request_type.as_deref(), schemas)
}

fn response_schema(method: &RpcMethodDef, schemas: &mut BTreeMap<String, Value>) -> Value {
  schema_ref(method.response_type.as_deref(), schemas)
}

fn schema_ref(type_name: Option<&str>, schemas: &mut BTreeMap<String, Value>) -> Value {
  match type_name {
    Some(name) => {
      schemas
        .entry(name.to_owned())
        .or_insert_with(|| json!({ "type": "object", "title": name }));
      json!({ "$ref": format!("#/components/schemas/{name}") })
    }
    None => json!({ "type": "object" }),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_method_has_a_name_and_a_key() {
    let all = [
      RpcHttpMethod::Get,
      RpcHttpMethod::Post,
      RpcHttpMethod::Put,
      RpcHttpMethod::Delete,
      RpcHttpMethod::Patch,
    ];
    let names: Vec<_> = all.iter().map(|m| m.as_str()).collect();
    assert_eq!(names, ["GET", "POST", "PUT", "DELETE", "PATCH"]);
    for method in all {
      assert_eq!(http_method_key(method), method.as_str().to_lowercase());
    }
  }

  #[test]
  fn operations_without_types_have_inline_object_schemas() {
    let spec = OpenApiSpec {
      info: OpenApiInfo {
        title: "Ping".to_owned(),
        version: "1".to_owned(),
      },
      service: RpcServiceDef {
        service: "Ping".to_owned(),
        methods: vec![RpcMethodDef {
          operation: "ping".to_owned(),
          path: "/ping".to_owned(),
          method: RpcHttpMethod::Get,
          summary: None,
          request_type: None,
          response_type: None,
        }],
      },
    };
    let v: Value = serde_json::from_str(&emit_openapi_json(&spec).unwrap()).unwrap();
    let op = &v["paths"]["/ping"]["get"];
    assert!(op.get("requestBody").is_none());
    assert!(op.get("summary").is_none());
    assert_eq!(
      op["responses"]["200"]["content"]["application/json"]["schema"]["type"],
      "object"
    );
    assert_eq!(v["components"], json!({}));
  }

  fn greet_spec() -> OpenApiSpec {
    OpenApiSpec {
      info: OpenApiInfo {
        title: "Greet API".to_owned(),
        version: "1.0.0".to_owned(),
      },
      service: RpcServiceDef {
        service: "GreetService".to_owned(),
        methods: vec![RpcMethodDef {
          operation: "greet".to_owned(),
          path: "/greet".to_owned(),
          method: RpcHttpMethod::Post,
          summary: Some("Say hello".to_owned()),
          request_type: Some("GreetRequest".to_owned()),
          response_type: Some("GreetResponse".to_owned()),
        }],
      },
    }
  }

  #[test]
  fn emit_openapi_json_matches_fixture_shape() {
    let json = emit_openapi_json(&greet_spec()).expect("json");
    let v: Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(v["openapi"], "3.0.3");
    assert_eq!(v["info"]["title"], "Greet API");
    assert_eq!(v["paths"]["/greet"]["post"]["operationId"], "greet");
    assert!(v["paths"]["/greet"]["post"]["requestBody"].is_object());
    assert_eq!(
      v["components"]["schemas"]["GreetRequest"]["title"],
      "GreetRequest"
    );
  }

  #[test]
  fn emit_openapi_yaml_contains_operation_id() {
    let yaml = emit_openapi_yaml(&greet_spec()).expect("yaml");
    assert!(yaml.contains("operationId: greet"));
    assert!(yaml.contains("title: Greet API"));
  }
}
