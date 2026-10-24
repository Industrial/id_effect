//! Admin forms: the bounded decoder of a submitted form body ([`decode_form`]), the
//! mass-assignment guard that binds only declared fields ([`FormBody::bind`]), and the typed
//! parsing and rendering of one field.
//!
//! **Secure by default** (ADR 0013 R1, R4):
//!
//! - A body over [`MAX_FORM_BYTES`] (64 KiB) is refused with 413 before it is decoded, and one
//!   with more than [`MAX_FORM_FIELDS`] pairs, invalid percent-encoding or non-UTF-8 text with
//!   400.
//! - Only the fields a form declares are bound: the editable fields of the entity
//!   (`#[entity(editable)]`, narrowed by [`EntityAdmin::form`](crate::EntityAdmin::form)). Any
//!   other submitted name — the primary key, a version or tenant column, a field the form does
//!   not show — is ignored and logged (target `id_effect_admin`, the field names only, never their
//!   values). The CSRF field and names starting with `_` (the admin's own: `_version`) are
//!   neither bound nor logged.
//!
//! ```
//! use id_effect_admin::forms::{FormError, MAX_FORM_BYTES, decode_form};
//!
//! let body = decode_form(b"title=Q3+report&secret=hunter2&csrf_token=abc").unwrap();
//! assert_eq!(body.value("title"), Some("Q3 report"));
//! let bound = body.bind(&["title"]);
//! assert_eq!(bound.values(), [("title".to_owned(), "Q3 report".to_owned())]);
//! assert_eq!(bound.ignored(), ["secret"]);
//!
//! assert_eq!(decode_form(&vec![b'a'; MAX_FORM_BYTES + 1]).unwrap_err(), FormError::TooLarge);
//! assert_eq!(decode_form(b"a=%zz").unwrap_err(), FormError::Malformed);
//! ```

use std::fmt;

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use id_effect_axum::server::security::CSRF_FORM_FIELD;
use id_effect_axum::ui::form::{FormState, InputKind};
use id_effect_axum::ui::{Markup, html};
use id_effect_sql::{FieldMeta, SqlValue};

use crate::value::{ValueKind, id_text};

/// The largest form body an admin page reads, in bytes (64 KiB).
pub const MAX_FORM_BYTES: usize = 64 * 1024;

/// The most `name=value` pairs a form body may carry.
pub const MAX_FORM_FIELDS: usize = 1024;

/// Why a form body was refused ([`decode_form`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FormError {
  /// Over [`MAX_FORM_BYTES`].
  TooLarge,
  /// More than [`MAX_FORM_FIELDS`] pairs.
  TooManyFields,
  /// Invalid percent-encoding, or text that is not UTF-8.
  Malformed,
}

impl FormError {
  /// The status an admin page answers with: 413 for [`TooLarge`](Self::TooLarge), 400 otherwise.
  pub fn status(self) -> StatusCode {
    match self {
      Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
      Self::TooManyFields | Self::Malformed => StatusCode::BAD_REQUEST,
    }
  }
}

impl fmt::Display for FormError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      Self::TooLarge => "form body is larger than the limit",
      Self::TooManyFields => "form body has more fields than the limit",
      Self::Malformed => "form body is not valid urlencoded UTF-8",
    })
  }
}

impl std::error::Error for FormError {}

/// A decoded `application/x-www-form-urlencoded` body: its `(name, value)` pairs in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormBody {
  pairs: Vec<(String, String)>,
}

/// The pairs of a [`FormBody`] a form declares, and the names it ignored ([`FormBody::bind`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoundForm {
  values: Vec<(String, String)>,
  ignored: Vec<String>,
}

impl BoundForm {
  /// The declared fields' pairs, in order.
  pub fn values(&self) -> &[(String, String)] {
    &self.values
  }

  /// The submitted names that are not declared fields, each once, in order (not the CSRF field
  /// nor the admin's `_`-prefixed names).
  pub fn ignored(&self) -> &[String] {
    &self.ignored
  }

  /// The last value of the declared field `name`, if submitted.
  pub fn value(&self, name: &str) -> Option<&str> {
    last(&self.values, name)
  }
}

fn last<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
  pairs
    .iter()
    .rev()
    .find(|(n, _)| n == name)
    .map(|(_, v)| v.as_str())
}

impl FormBody {
  /// Every pair, in order.
  pub fn pairs(&self) -> &[(String, String)] {
    &self.pairs
  }

  /// The last value of `name`.
  pub fn value(&self, name: &str) -> Option<&str> {
    last(&self.pairs, name)
  }

  /// Every value of `name`, in order (a repeated key: the selected rows of a bulk action).
  pub fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    self
      .pairs
      .iter()
      .filter(move |(n, _)| n == name)
      .map(|(_, v)| v.as_str())
  }

  /// The mass-assignment guard: the pairs whose name is one of `fields`, and the other names
  /// (ignored; the CSRF field and `_`-prefixed names are neither bound nor reported).
  pub fn bind(&self, fields: &[&str]) -> BoundForm {
    let mut bound = BoundForm::default();
    for (name, value) in &self.pairs {
      if fields.contains(&name.as_str()) {
        bound.values.push((name.clone(), value.clone()));
      } else if name != CSRF_FORM_FIELD && !name.starts_with('_') && !bound.ignored.contains(name) {
        bound.ignored.push(name.clone());
      }
    }
    bound
  }
}

/// Decode an `application/x-www-form-urlencoded` body (untrusted bytes; the fuzz target
/// `admin_form_body`): at most [`MAX_FORM_BYTES`] bytes and [`MAX_FORM_FIELDS`] pairs, `+` as a
/// space, `%XX` escapes, UTF-8 text. Empty pieces (`a=1&&b=2`) are skipped and a piece without
/// `=` is a name with an empty value.
///
/// # Errors
///
/// [`FormError`] when the body is too large, has too many pairs or does not decode.
pub fn decode_form(body: &[u8]) -> Result<FormBody, FormError> {
  if body.len() > MAX_FORM_BYTES {
    return Err(FormError::TooLarge);
  }
  let mut pairs = Vec::new();
  for piece in body.split(|b| *b == b'&').filter(|p| !p.is_empty()) {
    if pairs.len() == MAX_FORM_FIELDS {
      return Err(FormError::TooManyFields);
    }
    let mut kv = piece.splitn(2, |b| *b == b'=');
    let name = percent_decode(kv.next().unwrap_or_default())?;
    let value = percent_decode(kv.next().unwrap_or_default())?;
    pairs.push((name, value));
  }
  Ok(FormBody { pairs })
}

fn percent_decode(raw: &[u8]) -> Result<String, FormError> {
  let mut out = Vec::with_capacity(raw.len());
  let mut i = 0;
  while let Some(&b) = raw.get(i) {
    match b {
      b'+' => out.push(b' '),
      b'%' => {
        let hex = raw.get(i + 1..i + 3).ok_or(FormError::Malformed)?;
        let hex = std::str::from_utf8(hex).map_err(|_| FormError::Malformed)?;
        out.push(u8::from_str_radix(hex, 16).map_err(|_| FormError::Malformed)?);
        i += 2;
      }
      other => out.push(other),
    }
    i += 1;
  }
  String::from_utf8(out).map_err(|_| FormError::Malformed)
}

/// Read and decode a request body: 415 unless urlencoded, 413 over [`MAX_FORM_BYTES`] (declared
/// or read), 400 when it does not decode.
pub(crate) async fn read_form(headers: &HeaderMap, body: Body) -> Result<FormBody, StatusCode> {
  let urlencoded = headers
    .get(header::CONTENT_TYPE)
    .and_then(|v| v.to_str().ok())
    .is_some_and(|ct| ct.starts_with("application/x-www-form-urlencoded"));
  if !urlencoded {
    return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
  }
  let declared = headers
    .get(header::CONTENT_LENGTH)
    .and_then(|v| v.to_str().ok())
    .and_then(|v| v.parse::<u64>().ok());
  if declared.is_some_and(|len| len > MAX_FORM_BYTES as u64) {
    return Err(StatusCode::PAYLOAD_TOO_LARGE);
  }
  let bytes = axum::body::to_bytes(body, MAX_FORM_BYTES)
    .await
    .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
  decode_form(&bytes).map_err(FormError::status)
}

/// The schema name of a kind in messages (`expected i64`, `expected i64, got null`).
fn kind_name(kind: ValueKind) -> &'static str {
  match kind {
    ValueKind::Bool => "bool",
    ValueKind::I16 | ValueKind::I32 | ValueKind::I64 => "i64",
    ValueKind::F32 | ValueKind::F64 => "f64",
    ValueKind::Uuid => "uuid",
    ValueKind::Date => "date",
    ValueKind::Timestamp => "timestamp",
    ValueKind::Text | ValueKind::Other => "string",
  }
}

/// Whether a form can parse a field of `meta`'s type.
pub(crate) fn parses(meta: &FieldMeta) -> bool {
  ValueKind::of(meta.schema_type).parses()
}

/// The submitted `text` of the field `meta` as its typed value, or the schema-style message of
/// what is wrong (`expected i64`, `expected i64, got null`, `must be a valid UUID`). Empty text
/// is NULL for a nullable field and the empty string for a text field.
pub(crate) fn parse_field(meta: &FieldMeta, text: &str) -> Result<SqlValue, String> {
  let kind = ValueKind::of(meta.schema_type);
  if text.trim().is_empty() {
    return if meta.nullable {
      Ok(SqlValue::Null)
    } else if kind == ValueKind::Text {
      Ok(SqlValue::Text(text.to_owned()))
    } else {
      Err(format!("expected {}, got null", kind_name(kind)))
    };
  }
  if kind == ValueKind::Text {
    return Ok(SqlValue::Text(text.to_owned()));
  }
  kind.parse(text).ok_or_else(|| match kind {
    ValueKind::Uuid => "must be a valid UUID".to_owned(),
    other => format!("expected {}", kind_name(other)),
  })
}

/// `value` as the text of a form control (`true` / `false` for booleans, empty for NULL).
pub(crate) fn form_text(value: &SqlValue) -> String {
  id_text(value)
}

/// The control of field `meta`, labelled `label`, showing `state`'s value and errors: a yes / no
/// select for a boolean, a number or date input, a text input otherwise.
pub(crate) fn input(
  state: &FormState,
  meta: &FieldMeta,
  label: &str,
  yes: &str,
  no: &str,
) -> Markup {
  let name = meta.name;
  match ValueKind::of(meta.schema_type) {
    ValueKind::Bool => {
      let id = state.field(name, label).id();
      let errors = state.errors(name);
      let invalid = !errors.is_empty();
      let current = state.value(name);
      html! {
        div.field.invalid[invalid] data-field=(name) {
          label for=(id) { (label) }
          select id=(id) name=(name) aria-invalid=[invalid.then_some("true")] {
            @if meta.nullable { option value="" selected[current.is_empty()] { "—" } }
            option value="true" selected[current == "true"] { (yes) }
            option value="false" selected[current == "false"] { (no) }
          }
          @if invalid {
            ul.field-errors { @for e in errors { li data-message-key=(e.key()) { (e.message()) } } }
          }
        }
      }
    }
    ValueKind::I16 | ValueKind::I32 | ValueKind::I64 | ValueKind::F32 | ValueKind::F64 => {
      html! { (state.field(name, label).kind(InputKind::Number)) }
    }
    ValueKind::Date => html! { (state.field(name, label).kind(InputKind::Date)) },
    _ => html! { (state.field(name, label)) },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const fn meta(kind: &'static str, nullable: bool) -> FieldMeta {
    FieldMeta::new("f", "f", kind).nullable(nullable)
  }

  #[test]
  fn decode_handles_escapes_and_shapes() {
    let body = decode_form(b"a=1&&b&c=x%20y+z&d=%C3%A4").unwrap();
    assert_eq!(
      body.pairs(),
      [
        ("a".to_owned(), "1".to_owned()),
        ("b".to_owned(), String::new()),
        ("c".to_owned(), "x y z".to_owned()),
        ("d".to_owned(), "ä".to_owned()),
      ]
    );
    assert_eq!(body.values("a").collect::<Vec<_>>(), ["1"]);
    assert_eq!(body.value("zz"), None);
    assert_eq!(decode_form(b"a=%4").unwrap_err(), FormError::Malformed);
    assert_eq!(decode_form(b"a=%C3").unwrap_err(), FormError::Malformed);
    assert_eq!(
      decode_form(b"a=%\xff\xff").unwrap_err(),
      FormError::Malformed
    );
    let many = "a=1&".repeat(MAX_FORM_FIELDS + 1);
    assert_eq!(
      decode_form(many.as_bytes()).unwrap_err(),
      FormError::TooManyFields
    );
    assert_eq!(FormError::TooManyFields.status(), StatusCode::BAD_REQUEST);
    assert_eq!(FormError::TooLarge.status(), StatusCode::PAYLOAD_TOO_LARGE);
    for e in [
      FormError::TooLarge,
      FormError::TooManyFields,
      FormError::Malformed,
    ] {
      assert!(e.to_string().starts_with("form body"));
    }
  }

  #[test]
  fn decode_is_capped() {
    let over = vec![b'a'; MAX_FORM_BYTES + 1];
    assert_eq!(decode_form(&over), Err(FormError::TooLarge));
    let bound = decode_form(b"a=1&b=2").unwrap().bind(&["a"]);
    assert_eq!(bound.values(), [("a".to_owned(), "1".to_owned())]);
  }

  #[tokio::test]
  async fn read_form_checks_type_and_length() {
    let form = |headers: &[(&'static str, &str)]| {
      let mut map = HeaderMap::new();
      for (name, value) in headers {
        map.insert(*name, value.parse().unwrap());
      }
      map
    };
    let urlencoded = ("content-type", "application/x-www-form-urlencoded");
    assert_eq!(
      read_form(&form(&[]), Body::from("a=1")).await,
      Err(StatusCode::UNSUPPORTED_MEDIA_TYPE)
    );
    let declared = form(&[urlencoded, ("content-length", "70000")]);
    assert_eq!(
      read_form(&declared, Body::from("a=1")).await,
      Err(StatusCode::PAYLOAD_TOO_LARGE)
    );
    let ok = read_form(&form(&[urlencoded]), Body::from("a=1"))
      .await
      .unwrap();
    assert_eq!(ok.value("a"), Some("1"));
    assert_eq!(
      read_form(&form(&[urlencoded]), Body::from("a=%")).await,
      Err(StatusCode::BAD_REQUEST)
    );
  }

  #[test]
  fn bind_reports_each_ignored_name_once() {
    let body = decode_form(b"t=1&x=2&x=3&_version=4&csrf_token=k&t=5").unwrap();
    let bound = body.bind(&["t"]);
    assert_eq!(bound.value("t"), Some("5"));
    assert_eq!(bound.value("x"), None);
    assert_eq!(bound.ignored(), ["x"]);
  }

  #[test]
  fn fields_parse_by_kind() {
    assert_eq!(parse_field(&meta("i64", false), "7"), Ok(SqlValue::I64(7)));
    assert_eq!(
      parse_field(&meta("i64", false), "x"),
      Err("expected i64".into())
    );
    assert_eq!(
      parse_field(&meta("f64", false), "x"),
      Err("expected f64".into())
    );
    assert_eq!(
      parse_field(&meta("uuid::Uuid", false), "x"),
      Err("must be a valid UUID".into())
    );
    assert_eq!(
      parse_field(&meta("i32", false), " "),
      Err("expected i64, got null".into())
    );
    assert_eq!(
      parse_field(&meta("bool", false), ""),
      Err("expected bool, got null".into())
    );
    assert_eq!(parse_field(&meta("i32", true), ""), Ok(SqlValue::Null));
    assert_eq!(
      parse_field(&meta("String", false), ""),
      Ok(SqlValue::Text(String::new()))
    );
    let long = "x".repeat(1000);
    assert_eq!(
      parse_field(&meta("String", false), &long),
      Ok(SqlValue::Text(long.clone()))
    );
    let names: Vec<&str> = [
      "bool",
      "i16",
      "f32",
      "uuid::Uuid",
      "jiff::civil::Date",
      "jiff::Timestamp",
      "String",
      "serde_json::Value",
    ]
    .iter()
    .map(|t| kind_name(ValueKind::of(t)))
    .collect();
    assert_eq!(
      names,
      [
        "bool",
        "i64",
        "f64",
        "uuid",
        "date",
        "timestamp",
        "string",
        "string"
      ]
    );
    assert!(parses(&meta("i64", false)) && !parses(&meta("serde_json::Value", false)));
    assert_eq!(form_text(&SqlValue::Bool(true)), "true");
  }

  #[test]
  fn inputs_by_kind() {
    let state = FormState::new()
      .with_value("f", "true")
      .with_error("f", "expected bool");
    let html = input(&state, &meta("bool", true), "F", "Yes", "No").into_string();
    assert!(html.contains(r#"<option value="true" selected>Yes</option>"#));
    assert!(html.contains("—"));
    assert!(html.contains("field-errors"));
    let html = input(&FormState::new(), &meta("bool", false), "F", "Yes", "No").into_string();
    assert!(!html.contains("—") && !html.contains("field-errors"));
    let number = input(&FormState::new(), &meta("i64", false), "F", "", "").into_string();
    assert!(number.contains(r#"type="number""#));
    let date = input(
      &FormState::new(),
      &meta("jiff::civil::Date", false),
      "F",
      "",
      "",
    )
    .into_string();
    assert!(date.contains(r#"type="date""#));
    let text = input(&FormState::new(), &meta("String", false), "F", "", "").into_string();
    assert!(text.contains(r#"type="text""#));
  }
}
