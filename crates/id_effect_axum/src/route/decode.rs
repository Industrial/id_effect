//! Minimal request decoding for typed routes, and the [`RouteRejection`] it answers with.
//!
//! Path parameters, query pairs and header values arrive as strings; they are lifted into an
//! [`Unknown`] object guided by the declared schema's [`SchemaAst`] (an `Integer` field gets
//! [`Unknown::I64`], an `Array` field collects repeated keys, …) and decoded with
//! [`Schema::decode_unknown_all`](id_effect::schema::Schema::decode_unknown_all), so every issue of
//! every part is reported at once. The JSON body goes through
//! [`unknown_from_json`](crate::json::unknown_from_json).
//!
//! Seam: the `r09-e4-1-extractors-problem` leaf replaces this module with the full extractors
//! (forms, multipart) and the `application/problem+json` (RFC 9457) error body.

use std::collections::BTreeMap;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Query, RawPathParams, Request};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use id_effect::schema::{ParseErrors, SchemaAst, SchemaKind, Unknown};
use serde::Serialize;

use super::{Input, RoutePart};

/// Nesting limit when forcing [`SchemaKind::Lazy`] nodes during coercion.
const MAX_LAZY_DEPTH: usize = 8;

/// Where in the request an [`Issue`] was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Location {
  /// A path parameter.
  Path,
  /// The query string.
  Query,
  /// A request header.
  Header,
  /// The JSON body.
  Body,
}

/// One decoding failure of a typed route's input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Issue {
  /// Request part that failed.
  #[serde(rename = "in")]
  pub location: Location,
  /// Dot-separated field path within that part (empty = the part itself).
  pub path: String,
  /// Human-readable reason.
  pub message: String,
}

/// A typed route rejected its input: **422** with every [`Issue`], or **400** when the body is not
/// JSON at all. The body is `{"errors": [{"in", "path", "message"}, …]}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteRejection {
  /// Response status.
  pub status: StatusCode,
  /// Every failure, in part order (path, query, header, body).
  pub issues: Vec<Issue>,
}

#[derive(Serialize)]
struct RejectionBody<'a> {
  errors: &'a [Issue],
}

impl IntoResponse for RouteRejection {
  fn into_response(self) -> Response {
    (
      self.status,
      Json(RejectionBody {
        errors: &self.issues,
      }),
    )
      .into_response()
  }
}

/// The schema AST with [`SchemaKind::Lazy`] forced and [`SchemaKind::Nullable`] unwrapped (a
/// present string is never null).
fn resolve(ast: &SchemaAst) -> SchemaAst {
  let mut ast = ast.clone();
  for _ in 0..MAX_LAZY_DEPTH {
    ast = match ast.kind {
      SchemaKind::Lazy(lazy) => lazy.force(),
      SchemaKind::Nullable(inner) => *inner,
      _ => return ast,
    };
  }
  ast
}

/// Lift one raw string to the [`Unknown`] the scalar `ast` expects; left a string when it does
/// not parse, so the schema reports the mismatch.
fn coerce(raw: &str, ast: &SchemaAst) -> Unknown {
  let ast = resolve(ast);
  let parsed = match &ast.kind {
    SchemaKind::Integer | SchemaKind::Literal(Unknown::I64(_)) => {
      raw.parse().ok().map(Unknown::I64)
    }
    SchemaKind::Number => raw.parse().ok().map(Unknown::F64),
    SchemaKind::Boolean => raw.parse().ok().map(Unknown::Bool),
    SchemaKind::Union(arms) => arms
      .iter()
      .map(|arm| coerce(raw, arm))
      .find(|u| !matches!(u, Unknown::String(_))),
    _ => None,
  };
  parsed.unwrap_or_else(|| Unknown::String(raw.to_owned()))
}

/// Values of one key: an `Array` schema collects all of them, any other keeps the last.
fn coerce_values(values: &[String], ast: &SchemaAst) -> Unknown {
  match resolve(ast).kind {
    SchemaKind::Array(item) => Unknown::Array(values.iter().map(|v| coerce(v, &item)).collect()),
    _ => values
      .last()
      .map_or(Unknown::Null, |last| coerce(last, ast)),
  }
}

/// String `pairs` (path parameters or query pairs) as the [`Unknown`] that `ast` decodes.
///
/// A struct or record schema gets an object keyed by name; any other (scalar) schema with exactly
/// one pair gets that value directly, so `.path::<AccountId>()` works for `/accounts/{id}`.
pub(crate) fn pairs_unknown(pairs: Vec<(String, String)>, ast: &SchemaAst) -> Unknown {
  let ast = resolve(ast);
  let field_ast = |name: &str| match &ast.kind {
    SchemaKind::Struct(fields) => fields
      .iter()
      .find(|f| f.name == name)
      .map_or_else(SchemaAst::any, |f| f.schema.clone()),
    SchemaKind::Record(inner) => (**inner).clone(),
    _ => SchemaAst::any(),
  };
  let object_like = matches!(
    ast.kind,
    SchemaKind::Struct(_) | SchemaKind::Record(_) | SchemaKind::Any
  );
  if !object_like && pairs.len() == 1 {
    return coerce(&pairs[0].1, &ast);
  }
  let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
  for (k, v) in pairs {
    grouped.entry(k).or_default().push(v);
  }
  Unknown::Object(
    grouped
      .into_iter()
      .map(|(k, values)| {
        let value = coerce_values(&values, &field_ast(&k));
        (k, value)
      })
      .collect(),
  )
}

/// Request headers as the [`Unknown`] object that `ast` decodes.
///
/// A struct schema looks each field up by name (case-insensitively, keyed by the field's own
/// name); any other schema gets every header under its lowercase name. Non-UTF-8 values are
/// skipped.
pub(crate) fn headers_unknown(headers: &HeaderMap, ast: &SchemaAst) -> Unknown {
  let text = |name: &str| -> Vec<String> {
    headers
      .get_all(name)
      .iter()
      .filter_map(|v| v.to_str().ok().map(str::to_owned))
      .collect()
  };
  let pairs: Vec<(String, String)> = match resolve(ast).kind {
    SchemaKind::Struct(fields) => fields
      .iter()
      .flat_map(|f| text(&f.name).into_iter().map(|v| (f.name.clone(), v)))
      .collect(),
    _ => headers
      .keys()
      .flat_map(|name| {
        text(name.as_str())
          .into_iter()
          .map(|v| (name.as_str().to_owned(), v))
      })
      .collect(),
  };
  pairs_unknown(pairs, ast)
}

/// Decode one declared part, recording its issues under `location`.
fn part<T: RoutePart>(
  issues: &mut Vec<Issue>,
  location: Location,
  part: &T,
  input: Option<&Unknown>,
) -> Option<T::Value> {
  match part.decode(input) {
    Ok(v) => Some(v),
    Err(ParseErrors { issues: found }) => {
      issues.extend(found.into_iter().map(|e| Issue {
        location,
        path: e.path,
        message: e.message,
      }));
      None
    }
  }
}

/// A rejection response, boxed (`Response` is large).
fn reject(rejection: impl IntoResponse) -> Box<Response> {
  Box::new(rejection.into_response())
}

/// Decode the typed input of a request: path, query, headers, then the JSON body (read only when
/// a body is declared). Axum's own extractor rejections (body too large, invalid UTF-8 in a path
/// parameter, …) are answered as they are.
pub(crate) async fn decode_input<P, Q, H, B>(
  parts: &super::Parts<P, Q, H, B>,
  req: Request,
) -> Result<Input<P::Value, Q::Value, H::Value, B::Value>, Box<Response>>
where
  P: RoutePart,
  Q: RoutePart,
  H: RoutePart,
  B: RoutePart,
{
  let (mut head, body) = req.into_parts();
  let mut issues = Vec::new();
  let mut status = StatusCode::UNPROCESSABLE_ENTITY;

  let path_in = match parts.path.schema() {
    Some(schema) => {
      let raw = RawPathParams::from_request_parts(&mut head, &())
        .await
        .map_err(reject)?;
      let pairs = raw
        .iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
      Some(pairs_unknown(pairs, schema.ast()))
    }
    None => None,
  };
  let path = part(&mut issues, Location::Path, &parts.path, path_in.as_ref());

  let query_in = match parts.query.schema() {
    Some(schema) => {
      let Query(pairs) = Query::<Vec<(String, String)>>::try_from_uri(&head.uri).map_err(reject)?;
      Some(pairs_unknown(pairs, schema.ast()))
    }
    None => None,
  };
  let query = part(
    &mut issues,
    Location::Query,
    &parts.query,
    query_in.as_ref(),
  );

  let headers_in = parts
    .headers
    .schema()
    .map(|schema| headers_unknown(&head.headers, schema.ast()));
  let headers = part(
    &mut issues,
    Location::Header,
    &parts.headers,
    headers_in.as_ref(),
  );

  let body_in = match parts.body.schema() {
    Some(_) => {
      let bytes = Bytes::from_request(Request::from_parts(head, body), &())
        .await
        .map_err(reject)?;
      match serde_json::from_slice(&bytes) {
        Ok(value) => Some(crate::json::unknown_from_json(value)),
        Err(e) => {
          status = StatusCode::BAD_REQUEST;
          issues.push(Issue {
            location: Location::Body,
            path: String::new(),
            message: e.to_string(),
          });
          None
        }
      }
    }
    None => None,
  };
  let body = if status == StatusCode::BAD_REQUEST {
    None
  } else {
    part(&mut issues, Location::Body, &parts.body, body_in.as_ref())
  };

  match (path, query, headers, body) {
    (Some(path), Some(query), Some(headers), Some(body)) if issues.is_empty() => Ok(Input {
      path,
      query,
      headers,
      body,
    }),
    _ => Err(reject(RouteRejection { status, issues })),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::http::HeaderValue;
  use id_effect::schema::{self, LazyAst};

  fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
      .iter()
      .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
      .collect()
  }

  #[test]
  fn coerce_follows_the_scalar_kind() {
    let int = SchemaAst::new(SchemaKind::Integer);
    assert_eq!(coerce("42", &int), Unknown::I64(42));
    assert_eq!(coerce("x", &int), Unknown::String("x".into()));
    let num = SchemaAst::new(SchemaKind::Number);
    assert_eq!(coerce("1.5", &num), Unknown::F64(1.5));
    let boolean = SchemaAst::new(SchemaKind::Boolean);
    assert_eq!(coerce("true", &boolean), Unknown::Bool(true));
    let nullable = SchemaAst::new(SchemaKind::Nullable(Box::new(int.clone())));
    assert_eq!(coerce("7", &nullable), Unknown::I64(7));
    let lazy = SchemaAst::new(SchemaKind::Lazy(LazyAst::new(None, || {
      SchemaAst::new(SchemaKind::Integer)
    })));
    assert_eq!(coerce("8", &lazy), Unknown::I64(8));
    let union = SchemaAst::new(SchemaKind::Union(vec![
      SchemaAst::new(SchemaKind::String),
      boolean,
    ]));
    assert_eq!(coerce("false", &union), Unknown::Bool(false));
    assert_eq!(coerce("abc", &union), Unknown::String("abc".into()));
    assert_eq!(
      coerce("3", &SchemaAst::new(SchemaKind::Literal(Unknown::I64(3)))),
      Unknown::I64(3)
    );
  }

  #[test]
  fn resolve_stops_after_the_lazy_depth_limit() {
    fn endless() -> SchemaAst {
      SchemaAst::new(SchemaKind::Lazy(LazyAst::new(None, endless)))
    }
    assert!(matches!(resolve(&endless()).kind, SchemaKind::Lazy(_)));
  }

  #[test]
  fn pairs_follow_struct_record_and_scalar_schemas() {
    let s = schema::struct_(
      "limit",
      schema::i64::<()>(),
      "tag",
      schema::array(schema::string::<()>()),
    );
    let u = pairs_unknown(
      pairs(&[("limit", "5"), ("tag", "a"), ("tag", "b"), ("extra", "x")]),
      s.ast(),
    );
    let Unknown::Object(map) = u else {
      panic!("object")
    };
    assert_eq!(map["limit"], Unknown::I64(5));
    assert_eq!(
      map["tag"],
      Unknown::Array(vec![
        Unknown::String("a".into()),
        Unknown::String("b".into())
      ])
    );
    assert_eq!(map["extra"], Unknown::String("x".into()));

    let r = schema::record(schema::i64::<()>());
    let Unknown::Object(map) = pairs_unknown(pairs(&[("a", "1")]), r.ast()) else {
      panic!("object")
    };
    assert_eq!(map["a"], Unknown::I64(1));

    let scalar = schema::i64::<()>();
    assert_eq!(
      pairs_unknown(pairs(&[("id", "9")]), scalar.ast()),
      Unknown::I64(9)
    );
    assert!(matches!(
      pairs_unknown(pairs(&[("a", "1"), ("b", "2")]), scalar.ast()),
      Unknown::Object(_)
    ));
    assert_eq!(coerce_values(&[], scalar.ast()), Unknown::Null);
  }

  #[test]
  fn headers_follow_struct_fields_or_take_everything() {
    let mut headers = HeaderMap::new();
    headers.insert("x-tenant", HeaderValue::from_static("7"));
    headers.append("x-tag", HeaderValue::from_static("a"));
    headers.append("x-tag", HeaderValue::from_static("b"));
    headers.append("x-many", HeaderValue::from_static("1"));
    headers.append("x-many", HeaderValue::from_static("2"));
    headers.insert("x-bad", HeaderValue::from_bytes(b"\xff").unwrap());

    let s = schema::struct3(
      "X-Tenant",
      schema::i64::<()>(),
      "x-tag",
      schema::array(schema::string::<()>()),
      "x-many",
      schema::i64::<()>(),
    );
    let Unknown::Object(map) = headers_unknown(&headers, s.ast()) else {
      panic!("object")
    };
    assert_eq!(map["X-Tenant"], Unknown::I64(7));
    assert_eq!(
      map["x-tag"],
      Unknown::Array(vec![
        Unknown::String("a".into()),
        Unknown::String("b".into())
      ])
    );
    assert_eq!(map["x-many"], Unknown::I64(2));

    let Unknown::Object(map) = headers_unknown(&headers, &SchemaAst::any()) else {
      panic!("object")
    };
    assert_eq!(map["x-tenant"], Unknown::String("7".into()));
    assert!(!map.contains_key("x-bad"));
  }
}
