//! Request decoding shared by typed routes and the [`extract`](super::extract) extractors, and the
//! [`RouteRejection`] they answer with.
//!
//! Path parameters, query pairs, form fields and header values arrive as strings; they are lifted
//! into an [`Unknown`] object guided by the declared schema's [`SchemaAst`] (an `Integer` field gets
//! [`Unknown::I64`], an `Array` field collects repeated keys, …) and decoded with
//! [`Schema::decode_unknown_all`](id_effect::schema::Schema::decode_unknown_all), so every issue of
//! every part is reported at once. A JSON body goes through
//! [`unknown_from_json`](crate::json::unknown_from_json).
//!
//! A required struct field that is absent from the string pairs defaults to `[]` when its schema
//! is an array (no `?tag=` means no tags) and to `null` when it is nullable
//! ([`schema::optional`](id_effect::schema::optional) decodes it as `None`); any other absent field
//! is reported as missing.

use std::collections::BTreeMap;

use axum::Form;
use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Multipart, Query, RawPathParams, Request};
use axum::http::request::Parts as RequestParts;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use id_effect::schema::{ParseErrors, SchemaAst, SchemaKind, Unknown};

use super::problem::{Issue, Location, Problem};
use super::registry::BodyKind;
use super::{Input, RoutePart};

/// Nesting limit when forcing [`SchemaKind::Lazy`] nodes during coercion.
const MAX_LAZY_DEPTH: usize = 8;

/// Why a typed route or extractor rejected a request.
#[derive(Debug)]
pub enum RouteRejection {
  /// The input did not decode: an `application/problem+json` [`Problem`] listing every
  /// [`Issue`] — **422**, or **400** when the body is not well-formed.
  Invalid(Box<Problem>),
  /// One of Axum's own extractor rejections (body too large, wrong form content type, invalid
  /// UTF-8 in a path parameter, …), answered as Axum answers it.
  Extractor(Box<Response>),
}

impl RouteRejection {
  /// The problem, when the input did not decode.
  pub fn problem(&self) -> Option<&Problem> {
    match self {
      RouteRejection::Invalid(problem) => Some(problem),
      RouteRejection::Extractor(_) => None,
    }
  }
}

impl From<Problem> for RouteRejection {
  fn from(problem: Problem) -> Self {
    RouteRejection::Invalid(Box::new(problem))
  }
}

impl IntoResponse for RouteRejection {
  fn into_response(self) -> Response {
    match self {
      RouteRejection::Invalid(problem) => problem.into_response(),
      RouteRejection::Extractor(response) => *response,
    }
  }
}

/// An Axum extractor rejection as a [`RouteRejection::Extractor`].
pub(crate) fn extractor(rejection: impl IntoResponse) -> RouteRejection {
  RouteRejection::Extractor(Box::new(rejection.into_response()))
}

/// The issues of one part, located.
pub(crate) fn issues(location: Location, errors: ParseErrors) -> impl Iterator<Item = Issue> {
  errors
    .issues
    .into_iter()
    .map(move |e| Issue::from_parse_error(location, &e))
}

/// The [`Problem`] for `issues` of a request to `uri`.
pub(crate) fn problem(status: StatusCode, issues: Vec<Issue>, uri: &Uri) -> Problem {
  Problem::invalid_input(status, issues).with_instance(uri.path())
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

/// What an absent key of a required field decodes from: `[]` for an array, `null` for a nullable
/// schema, otherwise nothing (the schema reports the missing field).
fn absent_default(ast: &SchemaAst) -> Option<Unknown> {
  let mut ast = ast.clone();
  for _ in 0..MAX_LAZY_DEPTH {
    ast = match ast.kind {
      SchemaKind::Lazy(lazy) => lazy.force(),
      SchemaKind::Nullable(_) => return Some(Unknown::Null),
      SchemaKind::Array(_) => return Some(Unknown::Array(Vec::new())),
      _ => return None,
    };
  }
  None
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

/// String `pairs` (path parameters, query pairs or form fields) as the [`Unknown`] that `ast`
/// decodes.
///
/// A struct or record schema gets an object keyed by name (absent required array / nullable
/// fields defaulted, see the module docs); any other (scalar) schema with exactly one pair gets
/// that value directly, so `.path::<AccountId>()` works for `/accounts/{id}`.
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
  let mut object: BTreeMap<String, Unknown> = grouped
    .into_iter()
    .map(|(k, values)| {
      let value = coerce_values(&values, &field_ast(&k));
      (k, value)
    })
    .collect();
  if let SchemaKind::Struct(fields) = &ast.kind {
    for field in fields.iter().filter(|f| f.required) {
      if !object.contains_key(&field.name)
        && let Some(default) = absent_default(&field.schema)
      {
        object.insert(field.name.clone(), default);
      }
    }
  }
  Unknown::Object(object)
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

/// Path parameters as the [`Unknown`] that `ast` decodes.
pub(crate) async fn path_unknown<S: Send + Sync>(
  head: &mut RequestParts,
  state: &S,
  ast: &SchemaAst,
) -> Result<Unknown, RouteRejection> {
  let raw = RawPathParams::from_request_parts(head, state)
    .await
    .map_err(extractor)?;
  let pairs = raw
    .iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
  Ok(pairs_unknown(pairs, ast))
}

/// The query string as the [`Unknown`] that `ast` decodes.
pub(crate) fn query_unknown(uri: &Uri, ast: &SchemaAst) -> Result<Unknown, RouteRejection> {
  let Query(pairs) = Query::<Vec<(String, String)>>::try_from_uri(uri).map_err(extractor)?;
  Ok(pairs_unknown(pairs, ast))
}

/// Whether the request declares a `multipart/form-data` body.
fn is_multipart(headers: &HeaderMap) -> bool {
  headers
    .get(header::CONTENT_TYPE)
    .and_then(|v| v.to_str().ok())
    .is_some_and(|ct| {
      ct.trim_start()
        .to_ascii_lowercase()
        .starts_with("multipart/form-data")
    })
}

/// The text fields of a form: `multipart/form-data` (file fields skipped) or, through Axum's
/// [`Form`], `application/x-www-form-urlencoded` (the query string for `GET`/`HEAD`).
async fn form_pairs<S: Send + Sync>(
  req: Request,
  state: &S,
) -> Result<Vec<(String, String)>, RouteRejection> {
  if !is_multipart(req.headers()) {
    let Form(pairs) = Form::<Vec<(String, String)>>::from_request(req, state)
      .await
      .map_err(extractor)?;
    return Ok(pairs);
  }
  let mut multipart = Multipart::from_request(req, state)
    .await
    .map_err(extractor)?;
  let mut pairs = Vec::new();
  while let Some(field) = multipart.next_field().await.map_err(extractor)? {
    if field.file_name().is_some() {
      continue;
    }
    let name = field.name().unwrap_or_default().to_owned();
    let text = field.text().await.map_err(extractor)?;
    pairs.push((name, text));
  }
  Ok(pairs)
}

/// The body as the [`Unknown`] that `ast` decodes, read as `kind`. The inner `Err` is the
/// issue of a JSON body that is not well-formed (a **400**).
pub(crate) async fn body_unknown<S: Send + Sync>(
  req: Request,
  state: &S,
  ast: &SchemaAst,
  kind: BodyKind,
) -> Result<Result<Unknown, Issue>, RouteRejection> {
  match kind {
    BodyKind::Json => {
      let bytes = Bytes::from_request(req, state).await.map_err(extractor)?;
      Ok(
        serde_json::from_slice(&bytes)
          .map(crate::json::unknown_from_json)
          .map_err(|e| Issue::new(Location::Body, "", e.to_string())),
      )
    }
    BodyKind::Form => Ok(Ok(pairs_unknown(form_pairs(req, state).await?, ast))),
  }
}

/// Decode one declared part, recording its issues under `location`.
fn part<T: RoutePart>(
  found: &mut Vec<Issue>,
  location: Location,
  part: &T,
  input: Option<&Unknown>,
) -> Option<T::Value> {
  match part.decode(input) {
    Ok(v) => Some(v),
    Err(errors) => {
      found.extend(issues(location, errors));
      None
    }
  }
}

/// A rejection response, boxed (`Response` is large).
fn reject(rejection: RouteRejection) -> Box<Response> {
  Box::new(rejection.into_response())
}

/// Decode the typed input of a request: path, query, headers, then the body (read only when one
/// is declared). Every issue of every part is collected into one [`Problem`]: **400** when the
/// body is not well-formed JSON, otherwise **422**. Axum's own extractor rejections are answered
/// as they are.
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
  let uri = head.uri.clone();
  let mut found = Vec::new();
  let mut status = StatusCode::UNPROCESSABLE_ENTITY;

  let path_in = match parts.path.schema() {
    Some(schema) => Some(
      path_unknown(&mut head, &(), schema.ast())
        .await
        .map_err(reject)?,
    ),
    None => None,
  };
  let path = part(&mut found, Location::Path, &parts.path, path_in.as_ref());

  let query_in = match parts.query.schema() {
    Some(schema) => Some(query_unknown(&head.uri, schema.ast()).map_err(reject)?),
    None => None,
  };
  let query = part(&mut found, Location::Query, &parts.query, query_in.as_ref());

  let headers_in = parts
    .headers
    .schema()
    .map(|schema| headers_unknown(&head.headers, schema.ast()));
  let headers = part(
    &mut found,
    Location::Header,
    &parts.headers,
    headers_in.as_ref(),
  );

  let body = match parts.body.schema() {
    Some(schema) => {
      let req = Request::from_parts(head, body);
      match body_unknown(req, &(), schema.ast(), parts.body_kind)
        .await
        .map_err(reject)?
      {
        Ok(input) => part(&mut found, Location::Body, &parts.body, Some(&input)),
        Err(malformed) => {
          status = StatusCode::BAD_REQUEST;
          found.push(malformed);
          None
        }
      }
    }
    None => part(&mut found, Location::Body, &parts.body, None),
  };

  match (path, query, headers, body) {
    (Some(path), Some(query), Some(headers), Some(body)) if found.is_empty() => Ok(Input {
      path,
      query,
      headers,
      body,
    }),
    _ => Err(reject(problem(status, found, &uri).into())),
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
  fn resolve_and_absent_default_stop_after_the_lazy_depth_limit() {
    fn endless() -> SchemaAst {
      SchemaAst::new(SchemaKind::Lazy(LazyAst::new(None, endless)))
    }
    assert!(matches!(resolve(&endless()).kind, SchemaKind::Lazy(_)));
    assert_eq!(absent_default(&endless()), None);
    let lazy_array = SchemaAst::new(SchemaKind::Lazy(LazyAst::new(None, || {
      schema::array(schema::i64::<()>()).ast().clone()
    })));
    assert_eq!(absent_default(&lazy_array), Some(Unknown::Array(vec![])));
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
  fn absent_struct_fields_default_by_kind() {
    let s = schema::struct3(
      "limit",
      schema::i64::<()>(),
      "tag",
      schema::array(schema::string::<()>()),
      "cursor",
      schema::optional(schema::string::<()>()),
    );
    let Unknown::Object(map) = pairs_unknown(Vec::new(), s.ast()) else {
      panic!("object")
    };
    assert!(!map.contains_key("limit"));
    assert_eq!(map["tag"], Unknown::Array(vec![]));
    assert_eq!(map["cursor"], Unknown::Null);
    let decoded = s.decode_unknown_all(&pairs_unknown(pairs(&[("limit", "1")]), s.ast()));
    assert_eq!(decoded.unwrap(), (1, vec![], None));
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

  #[test]
  fn multipart_is_detected_from_the_content_type() {
    let mut headers = HeaderMap::new();
    assert!(!is_multipart(&headers));
    headers.insert(
      header::CONTENT_TYPE,
      HeaderValue::from_static(" Multipart/Form-Data; boundary=x"),
    );
    assert!(is_multipart(&headers));
  }

  #[test]
  fn rejections_expose_their_problem() {
    let invalid = RouteRejection::from(Problem::new(StatusCode::UNPROCESSABLE_ENTITY));
    assert_eq!(
      invalid.problem().map(|p| p.status),
      Some(StatusCode::UNPROCESSABLE_ENTITY)
    );
    let axum = extractor(StatusCode::PAYLOAD_TOO_LARGE);
    assert!(axum.problem().is_none());
    assert_eq!(axum.into_response().status(), StatusCode::PAYLOAD_TOO_LARGE);
  }
}
