//! Translated `Schema` validation messages (r13-e9-4-i18n-web): eco.i18n #5.
#![cfg(feature = "schema")]

use std::collections::BTreeMap;

use id_effect::schema::{
  ParseError, ParseErrors, Schema, Unknown, array, email, i64, max_items, maximum, min_length,
  one_of, refine, string, url,
};
use id_effect::{Effect, Exit, HCons, HNil, Never, hlist, run_test};
use id_effect_i18n::schema_messages::{SCHEMA_MESSAGES_DE, SCHEMA_MESSAGES_EN};
use id_effect_i18n::{
  Catalog, CurrentLocale, Locale, Localizer, SchemaErrorCode, SchemaMessages,
  localize_parse_errors, translate_parse_error, translate_parse_errors, with_locale,
};

fn loc(tag: &str) -> Locale {
  Locale::parse(tag).expect("test locale")
}

fn catalog() -> Catalog {
  Catalog::builder(loc("en"))
    .schema_messages()
    .source(
      loc("en"),
      "app/en.ftl",
      "age-positive = Age must be positive.",
    )
    .source(
      loc("de"),
      "app/de.ftl",
      "age-positive = Das Alter muss positiv sein.",
    )
    .use_isolating(false)
    .require_complete()
    .build()
    .expect("shipped catalogs are complete")
}

/// A signup form: `name` (≥ 3 chars), `email`, `age` (> 0, ≤ 150), `site` (URL), `plan`
/// (one of), `tags` (≤ 2), decoded field by field with every issue collected.
fn decode_signup(input: &BTreeMap<String, Unknown>) -> ParseErrors {
  fn check<A, I>(
    issues: &mut Vec<ParseError>,
    input: &BTreeMap<String, Unknown>,
    name: &str,
    schema: &Schema<A, I>,
  ) {
    match input.get(name) {
      None => issues.push(ParseError::new(name, format!("missing field {name}"))),
      Some(value) => {
        if let Err(errors) = schema.decode_unknown_all(value) {
          issues.extend(errors.prefix(name).issues);
        }
      }
    }
  }
  let mut issues = Vec::new();
  check(&mut issues, input, "name", &min_length(string(), 3));
  check(&mut issues, input, "email", &email(string()));
  check(
    &mut issues,
    input,
    "age",
    &maximum(refine(i64(), |n| *n > 0, "age-positive"), 150),
  );
  check(&mut issues, input, "site", &url(string()));
  check(
    &mut issues,
    input,
    "plan",
    &one_of(string(), &["free", "pro"]),
  );
  check(&mut issues, input, "tags", &max_items(array(string()), 2));
  check(&mut issues, input, "missing", &string());
  ParseErrors::new(issues)
}

fn signup_input() -> BTreeMap<String, Unknown> {
  let s = |v: &str| Unknown::String(v.to_owned());
  BTreeMap::from([
    ("name".to_owned(), s("Al")),
    ("email".to_owned(), s("not-an-email")),
    ("age".to_owned(), Unknown::I64(-3)),
    ("site".to_owned(), s("example")),
    ("plan".to_owned(), s("gold")),
    (
      "tags".to_owned(),
      Unknown::Array(vec![s("a"), s("b"), s("c")]),
    ),
  ])
}

fn messages(errors: &ParseErrors) -> Vec<(&str, &str)> {
  errors
    .issues
    .iter()
    .map(|issue| (issue.path.as_str(), issue.message.as_str()))
    .collect()
}

#[test]
fn validation_errors_translated_de() {
  let errors = decode_signup(&signup_input());
  let german = translate_parse_errors(&catalog(), &loc("de"), &errors);
  assert_eq!(
    messages(&german),
    [
      ("name", "Muss mindestens 3 Zeichen lang sein."),
      ("email", "Muss eine gültige E-Mail-Adresse sein."),
      ("age", "Das Alter muss positiv sein."),
      ("site", "Muss eine gültige URL sein."),
      ("plan", "Muss einer der folgenden Werte sein: free, pro."),
      ("tags", "Darf höchstens 2 Einträge enthalten."),
      ("missing", "Dieses Feld ist erforderlich."),
    ]
  );
  // A regional locale falls back to its language.
  let austrian = translate_parse_errors(&catalog(), &loc("de-AT"), &errors);
  assert_eq!(german.issues.len(), austrian.issues.len());
  assert_eq!(messages(&austrian), messages(&german));
}

#[test]
fn validation_errors_translated_en_and_untranslated_kept() {
  let errors = decode_signup(&signup_input());
  let english = translate_parse_errors(&catalog(), &loc("en"), &errors);
  assert_eq!(
    messages(&english)[..3],
    [
      ("name", "Must be at least 3 characters long."),
      ("email", "Must be a valid email address."),
      ("age", "Age must be positive."),
    ]
  );
  // A custom message without a translation stays as it was; so does everything when the catalog
  // has no schema messages at all.
  let issue = ParseError::new("x", "must be a palindrome");
  assert_eq!(
    translate_parse_error(&catalog(), &loc("de"), &issue).message,
    "must be a palindrome"
  );
  let bare = Catalog::builder(loc("en"))
    .embedded(loc("en"), "en.ftl", "k = v")
    .build()
    .expect("catalog");
  let kept = translate_parse_errors(&bare, &loc("de"), &errors);
  assert_eq!(kept.issues[0].message, "must be at least 3 characters");
}

#[test]
fn every_decoder_message_shape_is_translated() {
  let catalog = catalog();
  let de = loc("de");
  let cases: [(Unknown, Schema<i64, i64>, &str); 2] = [
    (Unknown::Null, i64(), "Ein Wert ist erforderlich."),
    (
      Unknown::String("1".into()),
      i64(),
      "Erwartet wird eine ganze Zahl.",
    ),
  ];
  for (input, schema, expected) in cases {
    let errors = schema.decode_unknown_all(&input).unwrap_err();
    assert_eq!(
      translate_parse_errors(&catalog, &de, &errors).issues[0].message,
      expected
    );
  }
  let errors = array(string::<()>())
    .decode_unknown_all(&Unknown::Bool(true))
    .unwrap_err();
  assert_eq!(
    translate_parse_errors(&catalog, &de, &errors).issues[0].message,
    "Erwartet wird eine Liste."
  );
  // Plurals of the shipped messages follow the locale.
  let one = ParseErrors::one(ParseError::new("t", "must have at least 1 items"));
  assert_eq!(
    translate_parse_errors(&catalog, &loc("en"), &one).issues[0].message,
    "Must have at least 1 item."
  );
  assert_eq!(
    translate_parse_errors(&catalog, &de, &one).issues[0].message,
    "Muss mindestens 1 Eintrag enthalten."
  );
  assert_eq!(
    SchemaErrorCode::classify("must be >= 1.5")
      .args("a")
      .get("min")
      .cloned(),
    Some(id_effect_i18n::ArgValue::Float(1.5))
  );
}

#[test]
fn localize_parse_errors_uses_the_current_locale() {
  type Env = HCons<Catalog, HCons<CurrentLocale, HNil>>;
  let errors = ParseErrors::one(ParseError::new("name", "must not be empty"));
  let eff: Effect<ParseErrors, Never, Env> = with_locale(loc("de"), localize_parse_errors(errors));
  let exit = run_test(eff.provide_all(hlist![catalog(), CurrentLocale::new(loc("en"))]));
  match exit {
    Exit::Success(translated) => assert_eq!(translated.issues[0].message, "Darf nicht leer sein."),
    other => panic!("{other:?}"),
  }
  let l = Localizer::new(catalog(), loc("en"));
  let errors = ParseErrors::one(ParseError::new("", "expected object"));
  assert_eq!(
    l.parse_errors(&errors).issues[0].message,
    "Expected an object."
  );
}

#[test]
fn shipped_sources_parse_and_have_the_same_keys() {
  assert!(SCHEMA_MESSAGES_EN.contains("schema-missing-field"));
  assert!(SCHEMA_MESSAGES_DE.contains("schema-missing-field"));
  // `require_complete` in `catalog()` proves de has every en key; and the reverse:
  let de_default = Catalog::builder(loc("de"))
    .schema_messages()
    .require_complete()
    .build();
  assert!(de_default.is_ok());
}
