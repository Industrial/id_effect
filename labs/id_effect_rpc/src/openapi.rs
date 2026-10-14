//! OpenAPI 3.0 document emission from RPC / HTTP route metadata.
//!
//! Build an [`OpenApiSpec`] from [`crate::codegen::RpcServiceDef`], then call
//! [`emit_openapi_json`] or [`emit_openapi_yaml`].

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::codegen::{RpcHttpMethod, RpcMethodDef, RpcServiceDef};

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
  use crate::codegen::RpcHttpMethod;

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
