//! HTML forms bound to [`Schema`](id_effect::schema::Schema) decode errors.
//!
//! [`HtmlForm<T>`] reads an `application/x-www-form-urlencoded` body (the query string for
//! `GET`), decodes it through `T`'s schema exactly like
//! [`ValidForm`](crate::route::ValidForm), and **does not reject** input that fails: it keeps the
//! submitted values and every issue bound to its field in a [`FormState`], so the handler
//! re-renders the form with what the user typed and what is wrong with it (answer `422`; the
//! htmx config of [`htmx_head`](super::htmx::htmx_head) swaps `422` responses in). Axum's own
//! rejections (body too large, wrong content type) still reject.
//!
//! [`FormState::field`] renders a labelled input with its value, `aria-invalid` and the error list.
//! Each [`FieldError`] carries a translatable message key (`schema-must-not-be-empty`, …) and its
//! numeric arguments; [`FormState::translate`] swaps in localized text (the i18n crate's catalogs
//! provide it), the English schema message is the fallback. Values and messages are escaped by
//! maud; password inputs never echo their value.
//!
//! ```rust
//! use axum::http::StatusCode;
//! use axum::response::{IntoResponse, Response};
//! use id_effect::schema::{self, HasSchema, Schema};
//! use id_effect_axum::ui::form::{FormState, HtmlForm, InputKind};
//! use id_effect_axum::ui::{Markup, html, maud};
//!
//! struct SignUp;
//!
//! impl HasSchema for SignUp {
//!   type A = (String, String);
//!   type I = (String, String);
//!   type E = ();
//!
//!   fn schema() -> Schema<Self::A, Self::I> {
//!     schema::struct_(
//!       "name",
//!       schema::non_empty(schema::string()),
//!       "email",
//!       schema::string(),
//!     )
//!   }
//! }
//!
//! fn form(state: &FormState) -> Markup {
//!   html! {
//!     form method="post" {
//!       (state.field("name", "Name"))
//!       (state.field("email", "E-mail").kind(InputKind::Email))
//!       button { "Sign up" }
//!     }
//!   }
//! }
//!
//! async fn sign_up(form: HtmlForm<SignUp>) -> Response {
//!   match form.into_result() {
//!     Ok(((name, _email), _)) => html! { p { "Welcome, " (name) } }.into_response(),
//!     Err(state) => (StatusCode::UNPROCESSABLE_ENTITY, form(&state)).into_response(),
//!   }
//! }
//! # let _ = axum::routing::post(sign_up);
//! let state = FormState::new()
//!   .with_value("name", "<b>")
//!   .with_error("name", "must not be empty");
//! let html = form(&state).into_string();
//! assert!(html.contains("value=\"&lt;b&gt;\""));
//! assert!(html.contains("data-message-key=\"schema-must-not-be-empty\""));
//! ```

use std::collections::BTreeMap;
use std::fmt;

use axum::Form;
use axum::body::{Body, Bytes};
use axum::extract::{FromRequest, Request};
use axum::response::{IntoResponse, Response};
use id_effect::schema::{HasSchema, ParseErrors};
use maud::{Markup, Render, html};

use crate::route::{Location, Problem, RouteRejection, ValidForm};
use crate::server::security::{CSRF_FORM_FIELD, CsrfToken};

/// Prefix of every message key derived from a schema message.
pub const MESSAGE_KEY_PREFIX: &str = "schema-";

/// Longest message key (longer slugs are cut at a word boundary).
const MAX_KEY_LEN: usize = 64;

/// One problem with one field: a stable message key for translation, its numeric arguments and
/// the text shown (the schema's English message until [`FormState::translate`] replaces it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldError {
  key: String,
  args: Vec<String>,
  message: String,
}

impl FieldError {
  /// The error for a schema `message`; its key is derived by [`message_key`].
  pub fn new(message: impl Into<String>) -> Self {
    let message = message.into();
    let (key, args) = message_key(&message);
    Self { key, args, message }
  }

  /// The translatable message key (a Fluent-compatible id, e.g. `schema-must-not-be-empty`).
  pub fn key(&self) -> &str {
    &self.key
  }

  /// The numbers taken out of the message, in order (`must be at most 120 characters` →
  /// `["120"]`); a translation receives the first as `$n`.
  pub fn args(&self) -> &[String] {
    &self.args
  }

  /// The text rendered for the user.
  pub fn message(&self) -> &str {
    &self.message
  }
}

impl fmt::Display for FieldError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.message)
  }
}

/// The message key and numeric arguments of a schema error message.
///
/// The key is [`MESSAGE_KEY_PREFIX`] plus the message as a lowercase slug in which every
/// standalone number becomes `n` (collected into the arguments): `must be at most 120
/// characters` → (`schema-must-be-at-most-n-characters`, `["120"]`), `expected i64` →
/// `schema-expected-i64`. A missing field is always `schema-missing-field` (the field is known
/// from where the error is shown). Keys are ASCII, at most 64 bytes, and valid Fluent message ids.
pub fn message_key(message: &str) -> (String, Vec<String>) {
  if message.starts_with("missing field") {
    return (format!("{MESSAGE_KEY_PREFIX}missing-field"), Vec::new());
  }
  let mut words = Vec::new();
  let mut args = Vec::new();
  for word in message
    .split(|c: char| !c.is_ascii_alphanumeric())
    .filter(|w| !w.is_empty())
  {
    if word.bytes().all(|b| b.is_ascii_digit()) {
      args.push(word.to_owned());
      words.push("n".to_owned());
    } else {
      words.push(word.to_ascii_lowercase());
    }
  }
  let mut key = MESSAGE_KEY_PREFIX.to_owned();
  for word in &words {
    if key.len() + word.len() + 1 > MAX_KEY_LEN {
      break;
    }
    if key.len() > MESSAGE_KEY_PREFIX.len() {
      key.push('-');
    }
    key.push_str(word);
  }
  if key.len() == MESSAGE_KEY_PREFIX.len() {
    key.push_str("invalid");
  }
  (key, args)
}

/// The field a JSON Pointer names (`/address/street` → `address.street`, `""` → the form).
fn field_of(pointer: &str) -> String {
  pointer
    .split('/')
    .skip(1)
    .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
    .collect::<Vec<_>>()
    .join(".")
}

/// A form's submitted values and the errors bound to its fields (`""` = the whole form).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormState {
  values: BTreeMap<String, Vec<String>>,
  errors: BTreeMap<String, Vec<FieldError>>,
}

impl FormState {
  /// Empty: no values, no errors (the form's first render).
  pub fn new() -> Self {
    Self::default()
  }

  /// The submitted `(name, value)` pairs (the CSRF field is left out: it is re-rendered fresh).
  pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
    let mut state = Self::new();
    for (name, value) in pairs {
      if name != CSRF_FORM_FIELD {
        state.values.entry(name).or_default().push(value);
      }
    }
    state
  }

  /// Set `name` to `value` (replacing earlier values).
  pub fn with_value(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
    self.values.insert(name.into(), vec![value.into()]);
    self
  }

  /// Bind an error `message` to `field` (`""` for the whole form).
  pub fn with_error(mut self, field: impl Into<String>, message: impl Into<String>) -> Self {
    self
      .errors
      .entry(field.into())
      .or_default()
      .push(FieldError::new(message));
    self
  }

  /// Bind the issues of a decode [`Problem`] (as typed routes and [`ValidForm`] answer it) to
  /// their fields; body and query issues count (a form arrives in one of them).
  pub fn bind_problem(mut self, problem: &Problem) -> Self {
    for issue in &problem.errors {
      if matches!(issue.location, Location::Body | Location::Query) {
        self = self.with_error(field_of(&issue.pointer), issue.message.clone());
      }
    }
    self
  }

  /// Bind schema [`ParseErrors`] (dot-separated paths are the field names).
  pub fn bind_parse_errors(mut self, errors: &ParseErrors) -> Self {
    for issue in &errors.issues {
      self = self.with_error(issue.path.clone(), issue.message.clone());
    }
    self
  }

  /// The last submitted value of `name` (`""` when absent).
  pub fn value(&self, name: &str) -> &str {
    self
      .values
      .get(name)
      .and_then(|v| v.last())
      .map_or("", String::as_str)
  }

  /// Every submitted value of `name` (repeated keys, multi-selects).
  pub fn values(&self, name: &str) -> &[String] {
    self.values.get(name).map_or(&[], Vec::as_slice)
  }

  /// The errors bound to `field`.
  pub fn errors(&self, field: &str) -> &[FieldError] {
    self.errors.get(field).map_or(&[], Vec::as_slice)
  }

  /// The errors of the whole form (not bound to a field).
  pub fn form_errors(&self) -> &[FieldError] {
    self.errors("")
  }

  /// Whether no error is bound.
  pub fn is_valid(&self) -> bool {
    self.errors.values().all(Vec::is_empty)
  }

  /// Number of errors, all fields together.
  pub fn error_count(&self) -> usize {
    self.errors.values().map(Vec::len).sum()
  }

  /// Replace each error's text with `translate(error)` where it returns `Some` (keys and
  /// arguments are kept); the schema's English message stays the fallback.
  pub fn translate(mut self, translate: impl Fn(&FieldError) -> Option<String>) -> Self {
    for error in self.errors.values_mut().flatten() {
      if let Some(text) = translate(error) {
        error.message = text;
      }
    }
    self
  }

  /// A labelled input for `name` showing its value and errors; see [`Field`].
  pub fn field<'a>(&'a self, name: &'a str, label: &'a str) -> Field<'a> {
    Field {
      state: self,
      name,
      label,
      kind: InputKind::Text,
      required: false,
      placeholder: None,
    }
  }

  /// The form-level errors as `<ul class="form-errors" role="alert">` (nothing when there are
  /// none).
  pub fn error_summary(&self) -> Markup {
    let errors = self.form_errors();
    html! {
      @if !errors.is_empty() {
        ul.form-errors role="alert" { @for e in errors { (error_item(e)) } }
      }
    }
  }
}

fn error_item(error: &FieldError) -> Markup {
  html! { li data-message-key=(error.key) { (error.message) } }
}

/// The hidden `csrf_token` input carrying `token` (masked) for a plain `<form method="post">`.
pub fn csrf_field(token: &CsrfToken) -> Markup {
  html! { input type="hidden" name=(CSRF_FORM_FIELD) value=(token.as_str()); }
}

/// The control [`Field`] renders.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputKind {
  /// `<input type="text">`.
  #[default]
  Text,
  /// `<input type="email">`.
  Email,
  /// `<input type="number">`.
  Number,
  /// `<input type="password">`; the submitted value is never rendered back.
  Password,
  /// `<input type="search">`.
  Search,
  /// `<input type="url">`.
  Url,
  /// `<input type="tel">`.
  Tel,
  /// `<input type="date">`.
  Date,
  /// `<textarea>`.
  TextArea,
}

impl InputKind {
  fn input_type(self) -> &'static str {
    match self {
      Self::Text | Self::TextArea => "text",
      Self::Email => "email",
      Self::Number => "number",
      Self::Password => "password",
      Self::Search => "search",
      Self::Url => "url",
      Self::Tel => "tel",
      Self::Date => "date",
    }
  }
}

/// A labelled form control bound to a [`FormState`] field; [`Render`] emits
///
/// ```html
/// <div class="field invalid" data-field="title">
///   <label for="field-title">Title</label>
///   <input id="field-title" name="title" type="text" value="…" aria-invalid="true"
///          aria-describedby="field-title-errors">
///   <ul class="field-errors" id="field-title-errors"><li data-message-key="…">…</li></ul>
/// </div>
/// ```
#[derive(Clone, Copy, Debug)]
#[must_use = "a field renders only when interpolated into markup"]
pub struct Field<'a> {
  state: &'a FormState,
  name: &'a str,
  label: &'a str,
  kind: InputKind,
  required: bool,
  placeholder: Option<&'a str>,
}

impl<'a> Field<'a> {
  /// Render as `kind`.
  pub fn kind(mut self, kind: InputKind) -> Self {
    self.kind = kind;
    self
  }

  /// Mark the control `required`.
  pub fn required(mut self) -> Self {
    self.required = true;
    self
  }

  /// Set a placeholder.
  pub fn placeholder(mut self, text: &'a str) -> Self {
    self.placeholder = Some(text);
    self
  }

  /// The element id: `field-` and the name with anything outside `[A-Za-z0-9_-]` as `-`.
  pub fn id(&self) -> String {
    let mut id = String::from("field-");
    id.extend(self.name.chars().map(|c| {
      if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
        c
      } else {
        '-'
      }
    }));
    id
  }
}

impl Render for Field<'_> {
  fn render(&self) -> Markup {
    let id = self.id();
    let errors = self.state.errors(self.name);
    let invalid = !errors.is_empty();
    let errors_id = format!("{id}-errors");
    let described = invalid.then_some(errors_id.as_str());
    let value = match self.kind {
      InputKind::Password => "",
      _ => self.state.value(self.name),
    };
    html! {
      div.field.invalid[invalid] data-field=(self.name) {
        label for=(id) { (self.label) }
        @if self.kind == InputKind::TextArea {
          textarea id=(id) name=(self.name) required[self.required]
            placeholder=[self.placeholder] aria-invalid=[invalid.then_some("true")]
            aria-describedby=[described] { (value) }
        } @else {
          input id=(id) name=(self.name) type=(self.kind.input_type()) value=(value)
            required[self.required] placeholder=[self.placeholder]
            aria-invalid=[invalid.then_some("true")] aria-describedby=[described];
        }
        @if invalid {
          ul.field-errors id=(errors_id) { @for e in errors { (error_item(e)) } }
        }
      }
    }
  }
}

/// A submitted form decoded through `T`'s schema, kept even when it does not decode; see the
/// [module docs](self).
pub struct HtmlForm<T: HasSchema> {
  value: Option<T::A>,
  state: FormState,
}

impl<T: HasSchema> fmt::Debug for HtmlForm<T>
where
  T::A: fmt::Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("HtmlForm")
      .field("value", &self.value)
      .field("state", &self.state)
      .finish()
  }
}

impl<T: HasSchema> HtmlForm<T> {
  /// The decoded value and the submitted state, or the state with every error bound.
  pub fn into_result(self) -> Result<(T::A, FormState), FormState> {
    match self.value {
      Some(value) => Ok((value, self.state)),
      None => Err(self.state),
    }
  }

  /// The submitted values and errors.
  pub fn state(&self) -> &FormState {
    &self.state
  }

  /// Whether the form decoded.
  pub fn is_valid(&self) -> bool {
    self.value.is_some()
  }
}

impl<T, S> FromRequest<S> for HtmlForm<T>
where
  T: HasSchema,
  T::A: Send,
  S: Send + Sync,
{
  type Rejection = Response;

  async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
    let (parts, body) = req.into_parts();
    let request = |body: Body| Request::from_parts(parts.clone(), body);
    let bytes = Bytes::from_request(request(body), state)
      .await
      .map_err(IntoResponse::into_response)?;
    let Form(pairs) =
      Form::<Vec<(String, String)>>::from_request(request(Body::from(bytes.clone())), state)
        .await
        .map_err(IntoResponse::into_response)?;
    let form = FormState::from_pairs(pairs);
    match ValidForm::<T>::from_request(request(Body::from(bytes)), state).await {
      Ok(ValidForm(value)) => Ok(Self {
        value: Some(value),
        state: form,
      }),
      Err(RouteRejection::Invalid(problem)) => Ok(Self {
        value: None,
        state: form.bind_problem(&problem),
      }),
      Err(other) => Err(other.into_response()),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::route::Issue;
  use axum::http::StatusCode;
  use id_effect::schema::{self, ParseError, Schema};

  #[test]
  fn message_keys_are_stable_slugs() {
    assert_eq!(
      message_key("must be at most 120 characters"),
      (
        "schema-must-be-at-most-n-characters".into(),
        vec!["120".into()]
      )
    );
    assert_eq!(message_key("expected i64").0, "schema-expected-i64");
    assert_eq!(message_key("missing field title").0, "schema-missing-field");
    assert_eq!(message_key("!!!").0, "schema-invalid");
    let long = message_key(&"word ".repeat(40)).0;
    assert!(
      long.len() <= MAX_KEY_LEN && long.ends_with("word"),
      "{long}"
    );
    let e = FieldError::new("must not be empty");
    assert_eq!(e.to_string(), "must not be empty");
    assert!(e.args().is_empty());
  }

  #[test]
  fn state_binds_problems_and_parse_errors() {
    let problem = Problem::invalid_input(
      StatusCode::UNPROCESSABLE_ENTITY,
      vec![
        Issue::new(Location::Body, "/title", "must not be empty"),
        Issue::new(Location::Query, "/a~1b/c", "bad"),
        Issue::new(Location::Header, "/x", "ignored"),
        Issue::new(Location::Body, "", "whole form"),
      ],
    );
    let state = FormState::from_pairs([
      ("title".into(), "".into()),
      (CSRF_FORM_FIELD.into(), "tok".into()),
      ("tag".into(), "a".into()),
      ("tag".into(), "b".into()),
    ])
    .bind_problem(&problem)
    .bind_parse_errors(&ParseErrors::one(ParseError::new("tag", "too many")));
    assert_eq!(state.errors("title")[0].key(), "schema-must-not-be-empty");
    assert_eq!(state.errors("a/b.c")[0].message(), "bad");
    assert!(state.errors("x").is_empty());
    assert_eq!(state.form_errors()[0].message(), "whole form");
    assert_eq!(state.values("tag"), ["a", "b"]);
    assert_eq!(state.value("tag"), "b");
    assert_eq!(state.value(CSRF_FORM_FIELD), "");
    assert_eq!(state.error_count(), 4);
    assert!(!state.is_valid());
    assert!(FormState::new().is_valid());
    let translated =
      state.translate(|e| (e.key() == "schema-must-not-be-empty").then(|| "leer".into()));
    assert_eq!(translated.errors("title")[0].message(), "leer");
    assert_eq!(translated.errors("tag")[0].message(), "too many");
    assert!(
      translated
        .error_summary()
        .into_string()
        .contains("role=\"alert\"")
    );
    assert_eq!(FormState::new().error_summary().into_string(), "");
  }

  #[test]
  fn fields_render_every_kind() {
    let state = FormState::new()
      .with_value("pw", "secret")
      .with_value("bio", "</textarea><script>")
      .with_value("a b", "1")
      .with_error("bio", "too long");
    let pw = state
      .field("pw", "Password")
      .kind(InputKind::Password)
      .render()
      .into_string();
    assert!(!pw.contains("secret"), "{pw}");
    let bio = state
      .field("bio", "Bio")
      .kind(InputKind::TextArea)
      .required()
      .placeholder("about you")
      .render()
      .into_string();
    assert!(bio.contains("&lt;/textarea&gt;&lt;script&gt;"), "{bio}");
    assert!(bio.contains("aria-invalid=\"true\""));
    assert!(bio.contains("aria-describedby=\"field-bio-errors\""));
    assert!(bio.contains("class=\"field invalid\""));
    assert!(bio.contains("required"));
    let spaced = state.field("a b", "AB");
    assert_eq!(spaced.id(), "field-a-b");
    for (kind, ty) in [
      (InputKind::Text, "text"),
      (InputKind::Email, "email"),
      (InputKind::Number, "number"),
      (InputKind::Search, "search"),
      (InputKind::Url, "url"),
      (InputKind::Tel, "tel"),
      (InputKind::Date, "date"),
    ] {
      let html = state.field("a b", "AB").kind(kind).render().into_string();
      assert!(html.contains(&format!("type=\"{ty}\"")), "{html}");
      assert!(!html.contains("aria-invalid"));
    }
  }

  struct Todo;

  impl HasSchema for Todo {
    type A = (String, Option<String>);
    type I = (String, Option<String>);
    type E = ();

    fn schema() -> Schema<Self::A, Self::I> {
      schema::struct_(
        "title",
        schema::max_length(schema::non_empty(schema::string()), 5),
        "tag",
        schema::optional(schema::string()),
      )
    }
  }

  fn post(body: &'static str, content_type: &str) -> Request {
    Request::post("/")
      .header("content-type", content_type)
      .body(Body::from(body))
      .unwrap()
  }

  const URLENCODED: &str = "application/x-www-form-urlencoded";

  #[tokio::test]
  async fn html_form_keeps_values_and_errors() {
    let ok = HtmlForm::<Todo>::from_request(post("title=milk&csrf_token=t", URLENCODED), &())
      .await
      .unwrap();
    assert!(ok.is_valid());
    assert!(format!("{ok:?}").contains("milk"));
    let ((title, tag), state) = ok.into_result().unwrap();
    assert_eq!((title.as_str(), tag), ("milk", None));
    assert_eq!(state.value("title"), "milk");

    let bad = HtmlForm::<Todo>::from_request(post("title=toolong", URLENCODED), &())
      .await
      .unwrap();
    assert!(!bad.is_valid());
    assert_eq!(bad.state().value("title"), "toolong");
    let state = bad.into_result().unwrap_err();
    assert_eq!(
      state.errors("title")[0].key(),
      "schema-must-be-at-most-n-characters"
    );
    assert_eq!(state.errors("title")[0].args(), ["5"]);

    let rejected = HtmlForm::<Todo>::from_request(post("{}", "application/json"), &())
      .await
      .unwrap_err();
    assert_eq!(rejected.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
  }
}
