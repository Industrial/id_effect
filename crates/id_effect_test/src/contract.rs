//! Contract testing (feature `contract`): checks that a [`TestResponse`] is what the app's OpenAPI
//! document says its operation answers.
//!
//! [`Contract::new`] takes an OpenAPI 3.1 document as JSON: what `id_effect_axum`'s
//! `OpenApi::to_json()` returns, or the app's own `/openapi.json` read with the
//! [`TestClient`](crate::http::TestClient). [`Contract::check`] then looks up, for one response:
//!
//! 1. the **operation** of the request that got it: its method and path, matched against the
//!    document's path templates (`/accounts/7` is `/accounts/{id}`; a literal segment beats a
//!    parameter);
//! 2. the **response** the operation declares for the status: the exact code, else its range
//!    (`4XX`), else `default` (a `$ref` to `#/components/responses` is followed);
//! 3. the **media type** of the response's `Content-Type` (`application/json`, else a declared
//!    `application/*` or `*/*`), and its JSON Schema;
//!
//! and validates the body against that schema (JSON Schema draft 2020-12, the dialect of OpenAPI
//! 3.1; `$ref`s into `#/components` resolve). A declared response without `content` must have an
//! empty body; bodies of non-JSON media types are not validated. Every mismatch is a
//! [`ContractError`] naming the operation and the status, and, for a body that does not match
//! its schema, the JSON pointer of each violation. [`TestResponse::assert_conforms`] panics with
//! that message and the body.
//!
//! ```rust
//! use axum::Json;
//! use axum::routing::get;
//! use id_effect::Exit;
//! use id_effect_test::contract::{Contract, ContractError};
//! use id_effect_test::http::TestClient;
//! use serde_json::json;
//!
//! let document = json!({
//!   "openapi": "3.1.0",
//!   "info": { "title": "accounts", "version": "1" },
//!   "paths": { "/accounts/{id}": { "get": { "responses": { "200": {
//!     "description": "OK",
//!     "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Account" } } }
//!   } } } } },
//!   "components": { "schemas": { "Account": {
//!     "type": "object",
//!     "required": ["id", "name"],
//!     "properties": { "id": { "type": "integer" }, "name": { "type": "string" } }
//!   } } }
//! });
//! let contract = Contract::new(document);
//!
//! let app = axum::Router::new()
//!   .route("/accounts/1", get(|| async { Json(json!({"id": 1, "name": "cash"})) }))
//!   .route("/accounts/2", get(|| async { Json(json!({"id": "2"})) }));
//! let client = TestClient::new(app);
//!
//! let Exit::Success(ok) = client.get("/accounts/1").send().block_on() else { panic!() };
//! ok.assert_conforms(&contract);
//!
//! let Exit::Success(drifted) = client.get("/accounts/2").send().block_on() else { panic!() };
//! let Err(ContractError::Schema { violations, .. }) = contract.check(&drifted) else { panic!() };
//! // `id` is not an integer (at `/id`) and `name` is missing (at the body itself).
//! let mut pointers: Vec<&str> = violations.iter().map(|v| v.pointer.as_str()).collect();
//! pointers.sort();
//! assert_eq!(pointers, ["", "/id"]);
//! assert!(contract.check(&drifted).unwrap_err().to_string().starts_with(
//!   "`GET /accounts/{id}` 200: the body does not match the declared schema"
//! ));
//! ```

use std::fmt;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::http::{Method, TestResponse, header};

/// An OpenAPI document responses are checked against (see the [module docs](self)). Cloning is
/// cheap.
#[derive(Clone)]
pub struct Contract {
  document: Arc<Value>,
}

impl fmt::Debug for Contract {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let paths = self
      .document
      .get("paths")
      .and_then(Value::as_object)
      .map_or(0, Map::len);
    f.debug_struct("Contract")
      .field("title", &self.document.pointer("/info/title"))
      .field("paths", &paths)
      .finish()
  }
}

/// One place a body does not match its schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
  /// JSON pointer into the body (`/accounts/0/id`; empty for the body itself).
  pub pointer: String,
  /// What is wrong there (`"7" is not of type "integer"`).
  pub message: String,
}

impl fmt::Display for Violation {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if self.pointer.is_empty() {
      write!(f, "at the body: {}", self.message)
    } else {
      write!(f, "at `{}`: {}", self.pointer, self.message)
    }
  }
}

/// A response that does not conform to the document. `operation` is `METHOD /path/{template}`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContractError {
  /// The document has no operation for the request's method and path.
  UnknownOperation {
    /// The request's method.
    method: String,
    /// The request's path.
    path: String,
  },
  /// The operation declares no response for the status (nor its range, nor `default`).
  UndeclaredStatus {
    /// The operation.
    operation: String,
    /// The response's status.
    status: u16,
    /// The response keys the operation declares.
    declared: Vec<String>,
  },
  /// The declared response has no `content`, but the response has a body.
  UnexpectedBody {
    /// The operation.
    operation: String,
    /// The response's status.
    status: u16,
  },
  /// The declared response has `content`, but the response has no body.
  MissingBody {
    /// The operation.
    operation: String,
    /// The response's status.
    status: u16,
    /// The declared media types.
    declared: Vec<String>,
  },
  /// The response's `Content-Type` is none of the declared media types.
  UnexpectedContentType {
    /// The operation.
    operation: String,
    /// The response's status.
    status: u16,
    /// The response's media type (empty without a `Content-Type`).
    content_type: String,
    /// The declared media types.
    declared: Vec<String>,
  },
  /// A JSON media type is declared, but the body is not JSON.
  InvalidJson {
    /// The operation.
    operation: String,
    /// The response's status.
    status: u16,
    /// The parser's message.
    message: String,
  },
  /// The body does not match the declared schema.
  Schema {
    /// The operation.
    operation: String,
    /// The response's status.
    status: u16,
    /// Every violation, in the validator's order.
    violations: Vec<Violation>,
  },
  /// The declared schema does not compile (an unresolvable `$ref`, an invalid keyword).
  InvalidDocument {
    /// The operation.
    operation: String,
    /// The validator's message.
    message: String,
  },
}

fn list(items: &[String]) -> String {
  if items.is_empty() {
    "none".to_owned()
  } else {
    items.join(", ")
  }
}

impl fmt::Display for ContractError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::UnknownOperation { method, path } => {
        write!(f, "the OpenAPI document has no operation `{method} {path}`")
      }
      Self::UndeclaredStatus {
        operation,
        status,
        declared,
      } => write!(
        f,
        "`{operation}` does not declare status {status} (declared: {})",
        list(declared)
      ),
      Self::UnexpectedBody { operation, status } => write!(
        f,
        "`{operation}` {status} declares no body, but the response has one"
      ),
      Self::MissingBody {
        operation,
        status,
        declared,
      } => write!(
        f,
        "`{operation}` {status} declares a body ({}), but the response has none",
        list(declared)
      ),
      Self::UnexpectedContentType {
        operation,
        status,
        content_type,
        declared,
      } => write!(
        f,
        "`{operation}` {status}: content type `{content_type}` is not declared (declared: {})",
        list(declared)
      ),
      Self::InvalidJson {
        operation,
        status,
        message,
      } => write!(f, "`{operation}` {status}: the body is not JSON: {message}"),
      Self::Schema {
        operation,
        status,
        violations,
      } => {
        write!(
          f,
          "`{operation}` {status}: the body does not match the declared schema"
        )?;
        for violation in violations {
          write!(f, "\n  {violation}")?;
        }
        Ok(())
      }
      Self::InvalidDocument { operation, message } => write!(
        f,
        "`{operation}`: the declared response schema does not compile: {message}"
      ),
    }
  }
}

impl std::error::Error for ContractError {}

/// Whether `template` (`/accounts/{id}`) matches `path` (`/accounts/7`); the number of literal
/// segments when it does.
fn template_match(template: &str, path: &str) -> Option<usize> {
  let (mut t, mut p) = (template.split('/'), path.split('/'));
  let mut literals = 0;
  loop {
    match (t.next(), p.next()) {
      (None, None) => return Some(literals),
      (Some(t), Some(p)) if t.len() > 2 && t.starts_with('{') && t.ends_with('}') => {
        if p.is_empty() {
          return None;
        }
      }
      (Some(t), Some(p)) if t == p => literals += 1,
      _ => return None,
    }
  }
}

/// The media type of a `Content-Type` value: no parameters, lower case.
fn media_type(content_type: &str) -> String {
  content_type
    .split(';')
    .next()
    .unwrap_or("")
    .trim()
    .to_ascii_lowercase()
}

fn is_json(media: &str) -> bool {
  media == "application/json" || media.ends_with("+json")
}

impl Contract {
  /// Responses checked against `document`, an OpenAPI 3.1 document as JSON.
  pub fn new(document: Value) -> Self {
    Self {
      document: Arc::new(document),
    }
  }

  /// The document.
  pub fn document(&self) -> &Value {
    &self.document
  }

  /// Checks `response` against the operation of the request that got it (its method and path).
  pub fn check(&self, response: &TestResponse) -> Result<(), ContractError> {
    self.check_operation(response.method(), response.uri().path(), response)
  }

  /// Checks `response` against the operation `method` `path` (a concrete path or a template).
  pub fn check_operation(
    &self,
    method: &Method,
    path: &str,
    response: &TestResponse,
  ) -> Result<(), ContractError> {
    let unknown = || ContractError::UnknownOperation {
      method: method.to_string(),
      path: path.to_owned(),
    };
    let (template, item) = self.path_item(path).ok_or_else(unknown)?;
    let op = self
      .resolve(item)
      .get(method.as_str().to_ascii_lowercase())
      .ok_or_else(unknown)?;
    let operation = format!("{method} {template}");
    let status = response.status();
    let code = status.as_u16();
    let responses = op.get("responses").and_then(Value::as_object);
    let range = format!("{}XX", code / 100);
    let declared = responses
      .and_then(|r| {
        r.get(status.as_str())
          .or_else(|| r.get(&range))
          .or_else(|| r.get(&range.to_ascii_lowercase()))
          .or_else(|| r.get("default"))
      })
      .ok_or_else(|| ContractError::UndeclaredStatus {
        operation: operation.clone(),
        status: code,
        declared: responses.map_or_else(Vec::new, |r| r.keys().cloned().collect()),
      })?;
    let content = self
      .resolve(declared)
      .get("content")
      .and_then(Value::as_object)
      .filter(|c| !c.is_empty());
    let body = response.bytes();
    let Some(content) = content else {
      return if body.is_empty() {
        Ok(())
      } else {
        Err(ContractError::UnexpectedBody {
          operation,
          status: code,
        })
      };
    };
    let media_types: Vec<String> = content.keys().cloned().collect();
    if body.is_empty() {
      return Err(ContractError::MissingBody {
        operation,
        status: code,
        declared: media_types,
      });
    }
    let media = media_type(response.header(header::CONTENT_TYPE.as_str()).unwrap_or(""));
    let wildcard = format!("{}/*", media.split('/').next().unwrap_or(""));
    let declared_media = content
      .iter()
      .find(|(k, _)| k.eq_ignore_ascii_case(&media))
      .or_else(|| content.iter().find(|(k, _)| *k == &wildcard || *k == "*/*"))
      .map(|(_, v)| v)
      .ok_or_else(|| ContractError::UnexpectedContentType {
        operation: operation.clone(),
        status: code,
        content_type: media.clone(),
        declared: media_types,
      })?;
    let Some(schema) = self.resolve(declared_media).get("schema") else {
      return Ok(());
    };
    if !is_json(&media) {
      return Ok(());
    }
    let instance: Value = serde_json::from_slice(body).map_err(|e| ContractError::InvalidJson {
      operation: operation.clone(),
      status: code,
      message: e.to_string(),
    })?;
    self.validate(&operation, code, schema, &instance)
  }

  /// `instance` against `schema`, with the document's `components` in scope of its `$ref`s.
  fn validate(
    &self,
    operation: &str,
    status: u16,
    schema: &Value,
    instance: &Value,
  ) -> Result<(), ContractError> {
    let components = self
      .document
      .get("components")
      .cloned()
      .unwrap_or_else(|| json!({}));
    let root = json!({ "allOf": [schema], "components": components });
    let validator =
      jsonschema::draft202012::new(&root).map_err(|e| ContractError::InvalidDocument {
        operation: operation.to_owned(),
        message: e.to_string(),
      })?;
    let violations: Vec<Violation> = validator
      .iter_errors(instance)
      .map(|e| Violation {
        pointer: e.instance_path().to_string(),
        message: e.to_string(),
      })
      .collect();
    if violations.is_empty() {
      Ok(())
    } else {
      Err(ContractError::Schema {
        operation: operation.to_owned(),
        status,
        violations,
      })
    }
  }

  /// The path item for `path`: the exact key, else the matching template with the most literal
  /// segments.
  fn path_item(&self, path: &str) -> Option<(&str, &Value)> {
    let paths = self.document.get("paths")?.as_object()?;
    if let Some((template, item)) = paths.get_key_value(path) {
      return Some((template.as_str(), item));
    }
    paths
      .iter()
      .filter_map(|(template, item)| {
        template_match(template, path).map(|literals| (literals, template.as_str(), item))
      })
      .max_by_key(|(literals, ..)| *literals)
      .map(|(_, template, item)| (template, item))
  }

  /// `value`, or what its local `$ref` (`#/components/…`) points to, followed a few times.
  fn resolve<'d>(&'d self, mut value: &'d Value) -> &'d Value {
    for _ in 0..8 {
      let target = value
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix('#'))
        .and_then(|pointer| self.document.pointer(pointer));
      match target {
        Some(target) => value = target,
        None => break,
      }
    }
    value
  }
}

impl TestResponse {
  /// Panics, naming the operation, the status and every violation (with its JSON pointer), and
  /// showing the body, unless the response conforms to `contract` ([`Contract::check`]);
  /// returns the response otherwise.
  #[track_caller]
  pub fn assert_conforms(&self, contract: &Contract) -> &Self {
    if let Err(e) = contract.check(self) {
      panic!(
        "response does not conform to the OpenAPI document: {e}\nbody: {}",
        self.text()
      );
    }
    self
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn templates_match_whole_segments() {
    // The empty segment before the leading `/` counts as a literal for both.
    assert_eq!(template_match("/accounts/{id}", "/accounts/7"), Some(2));
    assert_eq!(template_match("/accounts/{id}", "/accounts/"), None);
    assert_eq!(template_match("/accounts/{id}", "/accounts/7/x"), None);
    assert_eq!(template_match("/accounts/me", "/accounts/me"), Some(3));
    assert_eq!(template_match("/accounts/{id}", "/users/7"), None);
    assert_eq!(template_match("/a/{x}", "/a"), None);
  }

  #[test]
  fn media_types_drop_parameters() {
    assert_eq!(
      media_type("Application/JSON; charset=utf-8"),
      "application/json"
    );
    assert!(is_json("application/problem+json") && !is_json("text/plain"));
  }

  #[test]
  fn errors_name_operation_status_and_pointer() {
    let schema = ContractError::Schema {
      operation: "GET /a".into(),
      status: 200,
      violations: vec![
        Violation {
          pointer: String::new(),
          message: "m1".into(),
        },
        Violation {
          pointer: "/x/0".into(),
          message: "m2".into(),
        },
      ],
    };
    assert_eq!(
      schema.to_string(),
      "`GET /a` 200: the body does not match the declared schema\n  at the body: m1\n  at `/x/0`: m2"
    );
    let undeclared = ContractError::UndeclaredStatus {
      operation: "GET /a".into(),
      status: 404,
      declared: vec![],
    };
    assert_eq!(
      undeclared.to_string(),
      "`GET /a` does not declare status 404 (declared: none)"
    );
    assert!(
      ContractError::InvalidDocument {
        operation: "GET /a".into(),
        message: "bad".into()
      }
      .to_string()
      .contains("does not compile: bad")
    );
    let op = || "GET /a".to_owned();
    let messages = [
      ContractError::UnknownOperation {
        method: "GET".into(),
        path: "/b".into(),
      },
      ContractError::UnexpectedBody {
        operation: op(),
        status: 204,
      },
      ContractError::MissingBody {
        operation: op(),
        status: 200,
        declared: vec!["application/json".into()],
      },
      ContractError::UnexpectedContentType {
        operation: op(),
        status: 200,
        content_type: "text/html".into(),
        declared: vec!["application/json".into(), "text/plain".into()],
      },
      ContractError::InvalidJson {
        operation: op(),
        status: 200,
        message: "EOF".into(),
      },
    ]
    .map(|e| e.to_string());
    assert_eq!(
      messages,
      [
        "the OpenAPI document has no operation `GET /b`",
        "`GET /a` 204 declares no body, but the response has one",
        "`GET /a` 200 declares a body (application/json), but the response has none",
        "`GET /a` 200: content type `text/html` is not declared (declared: application/json, text/plain)",
        "`GET /a` 200: the body is not JSON: EOF",
      ]
    );
  }
}
