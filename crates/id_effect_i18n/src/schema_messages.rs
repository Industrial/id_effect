//! Translated `Schema` validation messages (feature `schema`; eco.i18n #5).
//!
//! `id_effect`'s schema decoders and validators report each issue as a [`ParseError`] with a path
//! and an English message, collected in [`ParseErrors`]. This module maps every message the
//! built-in decoders, validators and `#[derive(Schema)]` produce to a [`SchemaErrorCode`], each
//! code to a Fluent message id (`schema-min-length`, `schema-missing-field`, …) with its
//! arguments, and ships English and German catalogs for them ([`SchemaMessages`]).
//!
//! ```
//! use id_effect::schema::{Unknown, parse::string, validators::min_length};
//! use id_effect_i18n::{Catalog, Locale, SchemaMessages, translate_parse_errors};
//!
//! let en = Locale::parse("en").unwrap();
//! let de = Locale::parse("de").unwrap();
//! let catalog = Catalog::builder(en).schema_messages().use_isolating(false).build().unwrap();
//!
//! let name = min_length(string::<()>(), 3);
//! let errors = name.decode_unknown_all(&Unknown::String("Al".into())).unwrap_err();
//! let german = translate_parse_errors(&catalog, &de, &errors);
//! assert_eq!(german.issues[0].message, "Muss mindestens 3 Zeichen lang sein.");
//! ```
//!
//! A message that is not one of the built-in shapes (a custom `refine` message) becomes
//! [`SchemaErrorCode::Other`]: its message id is the message itself, so an application can use a
//! message id as the `refine` message and translate it in its own catalog
//! (`refine(s, pred, "age-positive")` with `age-positive = …` in each locale). Without a
//! translation the original message is kept.
//!
//! Formatted messages are text: render them through maud (escaped) or a JSON encoder, never as
//! raw HTML. Arguments carry the catalog's bidi isolation marks unless it was built with
//! `use_isolating(false)`.

use std::borrow::Cow;

use id_effect::schema::{ParseError, ParseErrors};
use id_effect::{Effect, Selector, need};

use crate::{
  ArgValue, Args, Catalog, CatalogBuilder, CurrentLocale, Locale, Localizer, current_locale,
};

/// The English messages for every [`SchemaErrorCode`] (Fluent source).
pub const SCHEMA_MESSAGES_EN: &str = include_str!("../locales/schema/en/schema.ftl");

/// The German messages for every [`SchemaErrorCode`] (Fluent source).
pub const SCHEMA_MESSAGES_DE: &str = include_str!("../locales/schema/de/schema.ftl");

/// The kind of a schema validation issue, recovered from its message.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaErrorCode {
  /// `missing field …`: a required object key is absent.
  MissingField,
  /// `expected …, got null`: a value is required.
  Required,
  /// `expected object`.
  ExpectedObject,
  /// `expected i64`.
  ExpectedInteger,
  /// `expected f64`.
  ExpectedNumber,
  /// `expected string`.
  ExpectedString,
  /// `expected bool`.
  ExpectedBoolean,
  /// `expected array`.
  ExpectedArray,
  /// `expected array of length N`.
  ExpectedArrayLength(u64),
  /// `expected literal V`: the only accepted value.
  ExpectedLiteral(String),
  /// `must not be empty`.
  NotEmpty,
  /// `must be at least N characters`.
  MinLength(u64),
  /// `must be at most N characters`.
  MaxLength(u64),
  /// `must match pattern P`.
  Pattern(String),
  /// `must be a valid email address`.
  Email,
  /// `must be a valid UUID`.
  Uuid,
  /// `must be a valid URL`.
  Url,
  /// `must be one of: A, B`.
  OneOf(String),
  /// `must be >= N`.
  Minimum(String),
  /// `must be <= N`.
  Maximum(String),
  /// `must be > N`.
  ExclusiveMinimum(String),
  /// `must be < N`.
  ExclusiveMaximum(String),
  /// `must have at least N items`.
  MinItems(u64),
  /// `must have at most N items`.
  MaxItems(u64),
  /// No alternative of a union accepted the value.
  NoMatch,
  /// Any other message (a custom `filter` / `refine` message), kept as is.
  Other(String),
}

impl SchemaErrorCode {
  /// Classifies a [`ParseError::message`].
  ///
  /// ```
  /// use id_effect_i18n::SchemaErrorCode;
  ///
  /// assert_eq!(SchemaErrorCode::classify("must be at least 3 characters"), SchemaErrorCode::MinLength(3));
  /// assert_eq!(SchemaErrorCode::classify("expected string, got null"), SchemaErrorCode::Required);
  /// assert_eq!(SchemaErrorCode::classify("age-positive").message_id(), "age-positive");
  /// ```
  pub fn classify(message: &str) -> Self {
    if message == "missing field" || message.starts_with("missing field ") {
      return Self::MissingField;
    }
    if message.starts_with("no arm of union_chain accepted") {
      return Self::NoMatch;
    }
    if let Some(rest) = message.strip_prefix("expected ")
      && let Some(code) = Self::expected(rest)
    {
      return code;
    }
    if let Some(rest) = message.strip_prefix("must ")
      && let Some(code) = Self::must(rest)
    {
      return code;
    }
    Self::Other(message.to_owned())
  }

  /// `expected …` messages (after the prefix).
  fn expected(rest: &str) -> Option<Self> {
    if rest.ends_with(", got null") {
      return Some(Self::Required);
    }
    Some(match rest {
      "object" | "object for record" => Self::ExpectedObject,
      "i64" => Self::ExpectedInteger,
      "f64" => Self::ExpectedNumber,
      "string" => Self::ExpectedString,
      "bool" => Self::ExpectedBoolean,
      "array" => Self::ExpectedArray,
      _ => {
        if let Some(n) = rest.strip_prefix("array of length ") {
          return n.parse().ok().map(Self::ExpectedArrayLength);
        }
        let value = rest.strip_prefix("literal ")?;
        let value = value.strip_prefix("i64 ").unwrap_or(value);
        Self::ExpectedLiteral(value.to_owned())
      }
    })
  }

  /// `must …` messages (after the prefix).
  fn must(rest: &str) -> Option<Self> {
    let count = |s: &str, prefix: &str, suffix: &str| -> Option<u64> {
      s.strip_prefix(prefix)?.strip_suffix(suffix)?.parse().ok()
    };
    let text = |s: &str, prefix: &str| s.strip_prefix(prefix).map(str::to_owned);
    match rest {
      "not be empty" => Some(Self::NotEmpty),
      "be a valid email address" => Some(Self::Email),
      "be a valid UUID" => Some(Self::Uuid),
      "be a valid URL" => Some(Self::Url),
      _ => count(rest, "be at least ", " characters")
        .map(Self::MinLength)
        .or_else(|| count(rest, "be at most ", " characters").map(Self::MaxLength))
        .or_else(|| count(rest, "have at least ", " items").map(Self::MinItems))
        .or_else(|| count(rest, "have at most ", " items").map(Self::MaxItems))
        .or_else(|| text(rest, "match pattern ").map(Self::Pattern))
        .or_else(|| text(rest, "be one of: ").map(Self::OneOf))
        .or_else(|| text(rest, "be >= ").map(Self::Minimum))
        .or_else(|| text(rest, "be <= ").map(Self::Maximum))
        .or_else(|| text(rest, "be > ").map(Self::ExclusiveMinimum))
        .or_else(|| text(rest, "be < ").map(Self::ExclusiveMaximum)),
    }
  }

  /// The Fluent message id; for [`Other`](Self::Other), the message itself.
  pub fn message_id(&self) -> &str {
    match self {
      Self::MissingField => "schema-missing-field",
      Self::Required => "schema-required",
      Self::ExpectedObject => "schema-expected-object",
      Self::ExpectedInteger => "schema-expected-integer",
      Self::ExpectedNumber => "schema-expected-number",
      Self::ExpectedString => "schema-expected-string",
      Self::ExpectedBoolean => "schema-expected-boolean",
      Self::ExpectedArray => "schema-expected-array",
      Self::ExpectedArrayLength(_) => "schema-expected-array-length",
      Self::ExpectedLiteral(_) => "schema-expected-literal",
      Self::NotEmpty => "schema-not-empty",
      Self::MinLength(_) => "schema-min-length",
      Self::MaxLength(_) => "schema-max-length",
      Self::Pattern(_) => "schema-pattern",
      Self::Email => "schema-email",
      Self::Uuid => "schema-uuid",
      Self::Url => "schema-url",
      Self::OneOf(_) => "schema-one-of",
      Self::Minimum(_) => "schema-minimum",
      Self::Maximum(_) => "schema-maximum",
      Self::ExclusiveMinimum(_) => "schema-exclusive-minimum",
      Self::ExclusiveMaximum(_) => "schema-exclusive-maximum",
      Self::MinItems(_) => "schema-min-items",
      Self::MaxItems(_) => "schema-max-items",
      Self::NoMatch => "schema-no-match",
      Self::Other(message) => message,
    }
  }

  /// The message arguments for an issue at `path`: `$path`, `$field` (the last path segment)
  /// and the code's own (`$min`, `$max`, `$length`, `$value`, `$pattern`, `$values`).
  pub fn args(&self, path: &str) -> Args {
    let field = path.rsplit('.').next().unwrap_or_default();
    let args = Args::new()
      .with("path", path.to_owned())
      .with("field", field.to_owned());
    match self {
      Self::ExpectedArrayLength(n) => args.with("length", *n),
      Self::MinLength(n) | Self::MinItems(n) => args.with("min", *n),
      Self::MaxLength(n) | Self::MaxItems(n) => args.with("max", *n),
      Self::ExpectedLiteral(v) => args.with("value", v.clone()),
      Self::Pattern(p) => args.with("pattern", p.clone()),
      Self::OneOf(v) => args.with("values", v.clone()),
      Self::Minimum(n) | Self::ExclusiveMinimum(n) => args.with("min", number(n)),
      Self::Maximum(n) | Self::ExclusiveMaximum(n) => args.with("max", number(n)),
      _ => args,
    }
  }
}

/// A bound as a number argument (so Fluent selectors see a number), or text when it is not one.
fn number(bound: &str) -> ArgValue {
  bound
    .parse::<i64>()
    .map(ArgValue::Int)
    .or_else(|_| bound.parse::<f64>().map(ArgValue::Float))
    .unwrap_or_else(|_| ArgValue::Text(bound.to_owned()))
}

/// Adds the shipped schema messages to a catalog: `en` ([`SCHEMA_MESSAGES_EN`]) and `de`
/// ([`SCHEMA_MESSAGES_DE`]).
///
/// The messages are added under the locales `en` and `de`, which makes those locales part of the
/// catalog. To reword a message, embed your own copy of the files instead (a message id defined
/// twice in one locale fails `build`).
pub trait SchemaMessages {
  /// This builder with the shipped schema messages added.
  #[must_use]
  fn schema_messages(self) -> Self;
}

impl SchemaMessages for CatalogBuilder {
  fn schema_messages(self) -> Self {
    let en = Locale::parse("en").expect("`en` is a valid locale");
    let de = Locale::parse("de").expect("`de` is a valid locale");
    self
      .embedded(
        en,
        "id_effect_i18n/schema/en/schema.ftl",
        SCHEMA_MESSAGES_EN,
      )
      .embedded(
        de,
        "id_effect_i18n/schema/de/schema.ftl",
        SCHEMA_MESSAGES_DE,
      )
  }
}

/// `issue` with its message translated into `locale` (same path). The original message is kept
/// when no locale of the fallback chain defines the code's message id.
pub fn translate_parse_error(catalog: &Catalog, locale: &Locale, issue: &ParseError) -> ParseError {
  let code = SchemaErrorCode::classify(&issue.message);
  let text = catalog
    .lookup(locale, code.message_id(), &code.args(&issue.path))
    .map_or_else(
      || Cow::Borrowed(issue.message.as_str()),
      |t| Cow::Owned(t.text),
    );
  ParseError::new(issue.path.clone(), text)
}

/// Every issue of `errors` translated into `locale` ([`translate_parse_error`]), in order.
pub fn translate_parse_errors(
  catalog: &Catalog,
  locale: &Locale,
  errors: &ParseErrors,
) -> ParseErrors {
  ParseErrors::new(
    errors
      .issues
      .iter()
      .map(|issue| translate_parse_error(catalog, locale, issue))
      .collect(),
  )
}

/// `errors` translated into the current fiber's locale, from the [`Catalog`] in the environment.
pub fn localize_parse_errors<E, R, J, K>(errors: ParseErrors) -> Effect<ParseErrors, E, R>
where
  E: Send + 'static,
  R: Selector<Catalog, J> + Selector<CurrentLocale, K> + Send + Sync + 'static,
{
  need::<Catalog>().flat_map(move |catalog| {
    current_locale::<E, R, K>()
      .map(move |locale| translate_parse_errors(&catalog, &locale, &errors))
  })
}

impl Localizer {
  /// `errors` translated into this locale ([`translate_parse_errors`]); feature `schema`.
  pub fn parse_errors(&self, errors: &ParseErrors) -> ParseErrors {
    translate_parse_errors(self.catalog(), self.locale(), errors)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_builtin_message_has_a_code_and_an_en_and_de_message() {
    let messages = [
      ("missing field name", SchemaErrorCode::MissingField),
      ("missing field", SchemaErrorCode::MissingField),
      ("expected i64, got null", SchemaErrorCode::Required),
      (
        "expected literal string, got null",
        SchemaErrorCode::Required,
      ),
      ("expected object", SchemaErrorCode::ExpectedObject),
      (
        "expected object for record",
        SchemaErrorCode::ExpectedObject,
      ),
      ("expected i64", SchemaErrorCode::ExpectedInteger),
      ("expected f64", SchemaErrorCode::ExpectedNumber),
      ("expected string", SchemaErrorCode::ExpectedString),
      ("expected bool", SchemaErrorCode::ExpectedBoolean),
      ("expected array", SchemaErrorCode::ExpectedArray),
      (
        "expected array of length 2",
        SchemaErrorCode::ExpectedArrayLength(2),
      ),
      (
        "expected literal \"red\"",
        SchemaErrorCode::ExpectedLiteral("\"red\"".into()),
      ),
      (
        "expected literal i64 7",
        SchemaErrorCode::ExpectedLiteral("7".into()),
      ),
      ("must not be empty", SchemaErrorCode::NotEmpty),
      (
        "must be at least 3 characters",
        SchemaErrorCode::MinLength(3),
      ),
      (
        "must be at most 9 characters",
        SchemaErrorCode::MaxLength(9),
      ),
      (
        "must match pattern ^a$",
        SchemaErrorCode::Pattern("^a$".into()),
      ),
      ("must be a valid email address", SchemaErrorCode::Email),
      ("must be a valid UUID", SchemaErrorCode::Uuid),
      ("must be a valid URL", SchemaErrorCode::Url),
      (
        "must be one of: red, green",
        SchemaErrorCode::OneOf("red, green".into()),
      ),
      ("must be >= 1", SchemaErrorCode::Minimum("1".into())),
      ("must be <= 1.5", SchemaErrorCode::Maximum("1.5".into())),
      ("must be > 0", SchemaErrorCode::ExclusiveMinimum("0".into())),
      ("must be < x", SchemaErrorCode::ExclusiveMaximum("x".into())),
      ("must have at least 1 items", SchemaErrorCode::MinItems(1)),
      ("must have at most 4 items", SchemaErrorCode::MaxItems(4)),
      (
        "no arm of union_chain accepted the wire value",
        SchemaErrorCode::NoMatch,
      ),
    ];
    let en = Locale::parse("en").unwrap();
    let de = Locale::parse("de").unwrap();
    let catalog = Catalog::builder(en.clone())
      .schema_messages()
      .require_complete()
      .build()
      .unwrap();
    for (message, code) in messages {
      assert_eq!(SchemaErrorCode::classify(message), code, "{message}");
      for locale in [&en, &de] {
        assert!(
          catalog.has_key(locale, code.message_id()),
          "{locale}: {}",
          code.message_id()
        );
      }
    }
  }

  #[test]
  fn unknown_shapes_are_other() {
    for message in [
      "expected array of length x",
      "expected something",
      "must be positive",
      "must be at least x characters",
      "bad value",
    ] {
      assert_eq!(
        SchemaErrorCode::classify(message),
        SchemaErrorCode::Other(message.into())
      );
    }
  }

  #[test]
  fn args_carry_path_field_and_bounds() {
    let args = SchemaErrorCode::Minimum("2.5".into()).args("user.age");
    assert_eq!(args.get("path"), Some(&ArgValue::Text("user.age".into())));
    assert_eq!(args.get("field"), Some(&ArgValue::Text("age".into())));
    assert_eq!(args.get("min"), Some(&ArgValue::Float(2.5)));
    let args = SchemaErrorCode::ExclusiveMaximum("n".into()).args("");
    assert_eq!(args.get("max"), Some(&ArgValue::Text("n".into())));
    assert_eq!(args.get("field"), Some(&ArgValue::Text(String::new())));
  }
}
