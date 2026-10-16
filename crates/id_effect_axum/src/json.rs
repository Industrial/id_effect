//! Decode JSON request bodies with [`id_effect::schema::Schema`].
//!
//! [`decode_json`] reports every schema issue at once as an RFC 9457 [`Problem`] (**422**, **400**
//! for a syntax error) — the body the typed routes and [`ValidJson`](crate::route::ValidJson)
//! answer with. The 0.8 [`decode_json_schema`] (first issue only, `{path, message}` body) is
//! deprecated and kept for one minor release.

// The deprecated 0.8 API below still names its own deprecated types.
#![allow(deprecated)]

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use id_effect::schema::data::EffectData;
use id_effect::schema::serde_bridge::unknown_from_serde_json;
use id_effect::schema::{ParseError, Schema, Unknown};
use serde::Serialize;
use serde_json::Value;

use crate::route::{Issue, Location, Problem};

/// JSON-shaped view of a schema or syntax failure (for clients).
#[deprecated(
  since = "0.9.0",
  note = "use `decode_json` (or the `route::ValidJson` extractor): failures are an RFC 9457 \
          `route::Problem` with every issue in `errors[]` as `{in, pointer, message}`"
)]
#[derive(Debug, Clone, Serialize)]
pub struct SchemaJsonErrorBody {
  /// JSON pointer–style path to the invalid value (empty for syntax errors).
  pub path: String,
  /// Human-readable error message for the client.
  pub message: String,
}

/// Failure while decoding a JSON body with [`decode_json_schema`].
#[deprecated(
  since = "0.9.0",
  note = "use `decode_json` (or the `route::ValidJson` extractor), which fails with an RFC 9457 \
          `route::Problem`"
)]
#[derive(Debug)]
pub enum JsonSchemaError {
  /// `serde_json` could not parse the bytes as JSON ([`BAD_REQUEST`](StatusCode::BAD_REQUEST)).
  Syntax(String),
  /// [`Schema::decode_unknown`] failed ([`UNPROCESSABLE_ENTITY`](StatusCode::UNPROCESSABLE_ENTITY)).
  Schema(ParseError),
}

impl JsonSchemaError {
  fn status(&self) -> StatusCode {
    match self {
      JsonSchemaError::Syntax(_) => StatusCode::BAD_REQUEST,
      JsonSchemaError::Schema(_) => StatusCode::UNPROCESSABLE_ENTITY,
    }
  }

  fn body(&self) -> SchemaJsonErrorBody {
    match self {
      JsonSchemaError::Syntax(msg) => SchemaJsonErrorBody {
        path: String::new(),
        message: msg.clone(),
      },
      JsonSchemaError::Schema(p) => SchemaJsonErrorBody {
        path: p.path.clone(),
        message: p.message.clone(),
      },
    }
  }
}

impl IntoResponse for JsonSchemaError {
  fn into_response(self) -> Response {
    let status = self.status();
    let body = self.body();
    (status, Json(body)).into_response()
  }
}

/// Turn [`serde_json::Value`] into [`Unknown`] for [`Schema::decode_unknown`].
///
/// Delegates to [`id_effect::schema::serde_bridge::unknown_from_serde_json`] so JSON mapping stays
/// consistent with `effect-reqwest` and the core crate.
pub fn unknown_from_json(value: Value) -> Unknown {
  unknown_from_serde_json(value)
}

/// Parse `bytes` as JSON, convert to [`Unknown`], then run [`Schema::decode_unknown_all`]: every
/// schema issue is reported at once.
///
/// The error is an RFC 9457 [`Problem`] (`application/problem+json`): **400** with one `body`
/// issue when `bytes` is not JSON, **422** with one `body` issue per failing value otherwise, each
/// with a JSON Pointer (`/items/0/name`). The schema's wire type `I` is unused.
///
/// ```rust
/// use axum::http::StatusCode;
/// use id_effect::schema;
/// use id_effect_axum::json::decode_json;
///
/// let person = schema::struct_("name", schema::string::<()>(), "age", schema::i64::<()>());
/// assert_eq!(
///   decode_json(&person, br#"{"name":"ada","age":36}"#).unwrap(),
///   ("ada".to_owned(), 36)
/// );
/// let problem = decode_json(&person, br#"{"name":1,"age":"x"}"#).unwrap_err();
/// assert_eq!(problem.status, StatusCode::UNPROCESSABLE_ENTITY);
/// assert_eq!(problem.errors.len(), 2);
/// assert_eq!(problem.errors[0].pointer, "/name");
/// ```
// `Problem` is the error body itself, returned by value like `JsonSchemaError` was.
#[allow(clippy::result_large_err)]
pub fn decode_json<A, I, E>(schema: &Schema<A, I, E>, bytes: &[u8]) -> Result<A, Problem>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  let value: Value = serde_json::from_slice(bytes).map_err(|e| {
    Problem::invalid_input(
      StatusCode::BAD_REQUEST,
      vec![Issue::new(Location::Body, "", e.to_string())],
    )
  })?;
  schema
    .decode_unknown_all(&unknown_from_json(value))
    .map_err(|errors| {
      Problem::invalid_input(
        StatusCode::UNPROCESSABLE_ENTITY,
        errors
          .issues
          .iter()
          .map(|e| Issue::from_parse_error(Location::Body, e))
          .collect(),
      )
    })
}

/// Parse `bytes` as JSON, convert to [`Unknown`], then run [`Schema::decode_unknown`].
///
/// The schema's wire type `I` is unused for this path (JSON is always parsed to [`Unknown`] first).
#[deprecated(
  since = "0.9.0",
  note = "use `decode_json` (or the `route::ValidJson` extractor): it reports every issue as an \
          RFC 9457 `route::Problem` (`application/problem+json`, `errors[]` of \
          `{in, pointer, message}`) instead of the first issue as `{path, message}`"
)]
pub fn decode_json_schema<A, I, E>(
  schema: &Schema<A, I, E>,
  bytes: &[u8],
) -> Result<A, JsonSchemaError>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  let value: Value =
    serde_json::from_slice(bytes).map_err(|e| JsonSchemaError::Syntax(e.to_string()))?;
  let unknown = unknown_from_json(value);
  schema
    .decode_unknown(&unknown)
    .map_err(JsonSchemaError::Schema)
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::Router;
  use axum::body::Body;
  use axum::http::Request;
  use axum::routing::post;
  use http_body_util::BodyExt;
  use id_effect::schema;
  use std::sync::Arc;
  use tower::ServiceExt;

  #[tokio::test]
  async fn axum_handler_returns_422_with_field_path_on_bad_body() {
    let person = Arc::new(schema::struct_(
      "name",
      schema::string::<()>(),
      "age",
      schema::i64::<()>(),
    ));
    let schema = person.clone();
    let app = Router::new().route(
      "/person",
      post(move |body: axum::body::Bytes| async move {
        match decode_json_schema(schema.as_ref(), &body) {
          Ok((name, age)) => (StatusCode::OK, format!("{name}:{age}")).into_response(),
          Err(e) => e.into_response(),
        }
      }),
    );

    let bad = br#"{"name":"ada","age":"nope"}"#;
    let req = Request::builder()
      .method("POST")
      .uri("/person")
      .header(axum::http::header::CONTENT_TYPE, "application/json")
      .body(Body::from(bad.as_slice()))
      .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    let path = v.get("path").and_then(|x| x.as_str()).unwrap_or("");
    assert!(
      path.contains("age"),
      "expected path to mention age, got {path:?} full={v:?}"
    );
  }

  #[test]
  fn decode_json_schema_syntax_error_is_bad_request() {
    let schema = Arc::new(schema::string::<()>());
    let err = decode_json_schema(schema.as_ref(), b"not-json-at-all").expect_err("syntax");
    assert!(matches!(err, JsonSchemaError::Syntax(_)));
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let n = decode_json_schema(&schema::i64::<()>(), b"42").expect("ok");
    assert_eq!(n, 42);
    let schema_err = decode_json_schema(schema.as_ref(), b"42").expect_err("type");
    assert!(matches!(schema_err, JsonSchemaError::Schema(_)));
    let _ = unknown_from_json(serde_json::json!("x"));
  }

  #[test]
  fn decode_json_answers_problems() {
    let schema = schema::array(schema::i64::<()>());
    assert_eq!(decode_json(&schema, b"[1,2]").unwrap(), vec![1, 2]);

    let syntax = decode_json(&schema, b"[1,").unwrap_err();
    assert_eq!(syntax.status, StatusCode::BAD_REQUEST);
    assert_eq!(syntax.errors[0].pointer, "");

    let invalid = decode_json(&schema, br#"[1,"a","b"]"#).unwrap_err();
    let pointers: Vec<&str> = invalid.errors.iter().map(|e| e.pointer.as_str()).collect();
    assert_eq!(pointers, ["/1", "/2"]);
    let res = invalid.into_response();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
      res.headers()[axum::http::header::CONTENT_TYPE],
      crate::route::PROBLEM_JSON
    );
  }
}
