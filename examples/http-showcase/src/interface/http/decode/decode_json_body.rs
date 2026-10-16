//! [`decode_json_body`]: a JSON request body, decoded through its `Schema`.

use axum::http::{HeaderMap, header};
use id_effect::schema::serde_bridge::unknown_from_serde_json;
use id_effect::schema::{HasSchema, Unknown};

use crate::interface::http::ApiError;

/// Decodes `body` into `T` through `T`'s schema: JSON content type, then JSON syntax, then the
/// schema, then no keys besides `T`'s fields. Any failure is `422 invalid`.
pub fn decode_json_body<T>(headers: &HeaderMap, body: &[u8]) -> Result<T, ApiError>
where
  T: HasSchema<A = T, I = Unknown> + Clone,
{
  let content_type = headers
    .get(header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .and_then(|value| value.split(';').next())
    .map(|essence| essence.trim().to_ascii_lowercase());
  let is_json = content_type.is_some_and(|essence| {
    essence
      .strip_prefix("application/")
      .is_some_and(|sub| sub == "json" || sub.ends_with("+json"))
  });
  if !is_json {
    return Err(ApiError::invalid(
      "expected a request with `Content-Type: application/json`",
    ));
  }
  let json: serde_json::Value = serde_json::from_slice(body)
    .map_err(|e| ApiError::invalid(format!("the body is not valid JSON: {e}")))?;
  let input = unknown_from_serde_json(json);
  let schema = <T as HasSchema>::schema();
  let value = schema.decode_unknown(&input).map_err(|e| {
    let at = if e.path.is_empty() {
      String::new()
    } else {
      format!(" at `{}`", e.path)
    };
    ApiError::invalid(format!("the body is invalid{at}: {}", e.message))
  })?;
  // The schema ignores extra keys; its encoding has exactly the known ones.
  if let (Unknown::Object(given), Unknown::Object(known)) = (&input, schema.encode(value.clone()))
    && let Some(extra) = given.keys().find(|key| !known.contains_key(*key))
  {
    return Err(ApiError::invalid(format!(
      "the body is invalid: unknown field `{extra}`"
    )));
  }
  Ok(value)
}

#[cfg(test)]
mod tests {
  use axum::http::HeaderValue;

  use super::*;
  use crate::interface::http::{CreateTodoRequest, CreateTodosRequest, UpdateTodoRequest};

  fn json_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
      header::CONTENT_TYPE,
      HeaderValue::from_static("application/json"),
    );
    headers
  }

  fn decode<T: HasSchema<A = T, I = Unknown> + Clone>(body: &str) -> Result<T, ApiError> {
    decode_json_body(&json_headers(), body.as_bytes())
  }

  #[test]
  fn decodes_the_bodies() {
    assert_eq!(
      decode::<CreateTodoRequest>(r#"{"title":"x"}"#),
      Ok(CreateTodoRequest { title: "x".into() })
    );
    assert_eq!(
      decode::<CreateTodosRequest>(r#"{"titles":["a","b"]}"#),
      Ok(CreateTodosRequest {
        titles: vec!["a".into(), "b".into()]
      })
    );
    assert_eq!(
      decode::<UpdateTodoRequest>(r#"{"title":"x"}"#),
      Ok(UpdateTodoRequest {
        title: Some("x".into()),
        completed: None
      })
    );
    assert_eq!(
      decode::<UpdateTodoRequest>(r#"{"completed":null}"#),
      Ok(UpdateTodoRequest::default())
    );
  }

  #[test]
  fn rejects_bad_bodies_with_422() {
    let cases = [
      ("{not json", "not valid JSON"),
      ("[]", "expected object"),
      (r#"{"name":"x"}"#, "`title`"),
      (r#"{"title":1}"#, "`title`"),
      (r#"{"title":"x","extra":true}"#, "unknown field `extra`"),
    ];
    for (body, needle) in cases {
      let err = decode::<CreateTodoRequest>(body).unwrap_err();
      assert_eq!((err.status.as_u16(), err.code), (422, "invalid"), "{body}");
      assert!(err.message.contains(needle), "{body}: {}", err.message);
    }
    let err = decode::<UpdateTodoRequest>(r#"{"completed":"yes"}"#).unwrap_err();
    assert!(err.message.contains("`completed`"), "{}", err.message);
  }

  #[test]
  fn requires_a_json_content_type() {
    let err = decode_json_body::<CreateTodoRequest>(&HeaderMap::new(), br#"{"title":"x"}"#);
    assert_eq!(err.unwrap_err().code, "invalid");
    let mut headers = HeaderMap::new();
    headers.insert(
      header::CONTENT_TYPE,
      HeaderValue::from_static("application/merge-patch+json; charset=utf-8"),
    );
    assert!(decode_json_body::<UpdateTodoRequest>(&headers, b"{}").is_ok());
  }
}
