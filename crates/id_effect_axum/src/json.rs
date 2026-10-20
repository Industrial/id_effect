//! Decode JSON request bodies with [`id_effect::schema::Schema`].
//!
//! [`decode_json`] reports every schema issue at once as an RFC 9457 [`Problem`] (**422**, **400**
//! for a syntax error) — the body the typed routes and [`ValidJson`](crate::route::ValidJson)
//! answer with.

use axum::http::StatusCode;
use id_effect::schema::data::EffectData;
use id_effect::schema::serde_bridge::unknown_from_serde_json;
use id_effect::schema::{Schema, Unknown};
use serde_json::Value;

use crate::route::{Issue, Location, Problem};

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
// `Problem` is the error body itself, returned by value.
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

#[cfg(test)]
mod tests {
  use super::*;
  use axum::Router;
  use axum::body::Body;
  use axum::http::Request;
  use axum::response::IntoResponse;
  use axum::routing::post;
  use http_body_util::BodyExt;
  use id_effect::schema;
  use std::sync::Arc;
  use tower::ServiceExt;

  #[tokio::test]
  async fn axum_handler_returns_422_with_field_pointer_on_bad_body() {
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
        match decode_json(schema.as_ref(), &body) {
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
    let pointer = v["errors"][0]["pointer"].as_str().unwrap_or("");
    assert_eq!(pointer, "/age", "full={v:?}");
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
