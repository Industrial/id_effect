//! Built-in validators: string length / pattern / `email` / `uuid` / `url` / `one_of`, numeric
//! ranges, array sizes.
//!
//! Each validator is a [`filter`] that **also** records a declarative constraint in the schema's
//! [`SchemaAst`](crate::schema::ast::SchemaAst), so the very schema value that validates input is
//! the one exported as JSON Schema (`minLength`, `pattern`, `format`, `enum`, `minimum`, …).
//! Stacking validators tightens the recorded bounds; decoding stops at the first failing one.
//!
//! String lengths count Unicode scalar values (`chars()`), as JSON Schema does.

use std::fmt;

use crate::schema::ast::{Constraints, tighten_max, tighten_min};
use crate::schema::data::EffectData;
use crate::schema::parse::{Schema, Unknown, filter};

/// Numeric types usable with [`minimum`], [`maximum`], [`range`] and the exclusive bounds.
pub trait NumericBound: PartialOrd + Copy + fmt::Display + Send + Sync + 'static {
  /// Bound as recorded in [`Constraints`] (JSON numbers).
  fn to_f64(self) -> f64;
}

impl NumericBound for i64 {
  #[allow(clippy::cast_precision_loss)] // JSON Schema bounds are JSON numbers
  fn to_f64(self) -> f64 {
    self as f64
  }
}

impl NumericBound for f64 {
  fn to_f64(self) -> f64 {
    self
  }
}

fn constrain<A, I, E>(
  s: Schema<A, I, E>,
  pred: impl Fn(&A) -> bool + Send + Sync + 'static,
  message: String,
  tighten: impl FnOnce(&mut Constraints),
) -> Schema<A, I, E>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  let mut ast = s.ast().clone();
  tighten(&mut ast.constraints);
  filter(s, pred, message).with_ast(ast)
}

/// At least `n` characters (JSON Schema `minLength`).
pub fn min_length<I, E>(s: Schema<String, I, E>, n: usize) -> Schema<String, I, E>
where
  E: EffectData + 'static,
  I: 'static,
{
  let message = if n == 1 {
    "must not be empty".to_owned()
  } else {
    format!("must be at least {n} characters")
  };
  constrain(
    s,
    move |v| v.chars().count() >= n,
    message,
    |c| {
      tighten_min(&mut c.min_length, n);
    },
  )
}

/// At most `n` characters (JSON Schema `maxLength`).
pub fn max_length<I, E>(s: Schema<String, I, E>, n: usize) -> Schema<String, I, E>
where
  E: EffectData + 'static,
  I: 'static,
{
  constrain(
    s,
    move |v| v.chars().count() <= n,
    format!("must be at most {n} characters"),
    |c| tighten_max(&mut c.max_length, n),
  )
}

/// Between `min` and `max` characters, inclusive.
pub fn length<I, E>(s: Schema<String, I, E>, min: usize, max: usize) -> Schema<String, I, E>
where
  E: EffectData + 'static,
  I: 'static,
{
  max_length(min_length(s, min), max)
}

/// Non-empty string (`minLength: 1`).
pub fn non_empty<I, E>(s: Schema<String, I, E>) -> Schema<String, I, E>
where
  E: EffectData + 'static,
  I: 'static,
{
  min_length(s, 1)
}

/// Must match the regular expression `source` (JSON Schema `pattern`).
///
/// The crate does not bundle a regex engine: `matches` performs the check (e.g.
/// `move |s| re.is_match(s)` with the `regex` crate) and `source` is what JSON Schema export emits.
/// Keep the two in sync — `source` should be the expression `matches` compiles.
pub fn pattern<I, E>(
  s: Schema<String, I, E>,
  source: impl Into<String>,
  matches: impl Fn(&str) -> bool + Send + Sync + 'static,
) -> Schema<String, I, E>
where
  E: EffectData + 'static,
  I: 'static,
{
  let source = source.into();
  let message = format!("must match pattern {source}");
  constrain(
    s,
    move |v| matches(v),
    message,
    |c| {
      c.pattern = Some(source);
    },
  )
}

/// Syntactically valid email address `local@domain.tld` (JSON Schema `format: email`).
///
/// Pragmatic check, not full RFC 5322: one `@`, non-empty local part without whitespace, and a
/// dotted domain of alphanumeric / `-` labels.
pub fn email<I, E>(s: Schema<String, I, E>) -> Schema<String, I, E>
where
  E: EffectData + 'static,
  I: 'static,
{
  constrain(
    s,
    |v| is_email(v),
    "must be a valid email address".to_owned(),
    |c| c.format = Some("email".to_owned()),
  )
}

/// Hyphenated UUID `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`, any version, either case (JSON Schema
/// `format: uuid`).
pub fn uuid<I, E>(s: Schema<String, I, E>) -> Schema<String, I, E>
where
  E: EffectData + 'static,
  I: 'static,
{
  constrain(
    s,
    |v| is_uuid(v),
    "must be a valid UUID".to_owned(),
    |c| c.format = Some("uuid".to_owned()),
  )
}

/// Absolute URL `scheme://authority[/path…]` (JSON Schema `format: uri`).
pub fn url<I, E>(s: Schema<String, I, E>) -> Schema<String, I, E>
where
  E: EffectData + 'static,
  I: 'static,
{
  constrain(
    s,
    |v| is_url(v),
    "must be a valid URL".to_owned(),
    |c| c.format = Some("uri".to_owned()),
  )
}

/// One of a closed set of strings (JSON Schema `enum`).
pub fn one_of<I, E>(s: Schema<String, I, E>, values: &[&str]) -> Schema<String, I, E>
where
  E: EffectData + 'static,
  I: 'static,
{
  let allowed: Vec<String> = values.iter().map(|v| (*v).to_owned()).collect();
  let message = format!("must be one of: {}", allowed.join(", "));
  let recorded: Vec<Unknown> = allowed.iter().cloned().map(Unknown::String).collect();
  constrain(
    s,
    move |v| allowed.contains(v),
    message,
    |c| {
      c.enum_values = Some(recorded);
    },
  )
}

/// `>= min` (JSON Schema `minimum`).
pub fn minimum<N, I, E>(s: Schema<N, I, E>, min: N) -> Schema<N, I, E>
where
  N: NumericBound,
  E: EffectData + 'static,
  I: 'static,
{
  constrain(
    s,
    move |v| *v >= min,
    format!("must be >= {min}"),
    |c| {
      tighten_min(&mut c.minimum, min.to_f64());
    },
  )
}

/// `<= max` (JSON Schema `maximum`).
pub fn maximum<N, I, E>(s: Schema<N, I, E>, max: N) -> Schema<N, I, E>
where
  N: NumericBound,
  E: EffectData + 'static,
  I: 'static,
{
  constrain(
    s,
    move |v| *v <= max,
    format!("must be <= {max}"),
    |c| {
      tighten_max(&mut c.maximum, max.to_f64());
    },
  )
}

/// `> min` (JSON Schema `exclusiveMinimum`).
pub fn exclusive_minimum<N, I, E>(s: Schema<N, I, E>, min: N) -> Schema<N, I, E>
where
  N: NumericBound,
  E: EffectData + 'static,
  I: 'static,
{
  constrain(
    s,
    move |v| *v > min,
    format!("must be > {min}"),
    |c| {
      tighten_min(&mut c.exclusive_minimum, min.to_f64());
    },
  )
}

/// `< max` (JSON Schema `exclusiveMaximum`).
pub fn exclusive_maximum<N, I, E>(s: Schema<N, I, E>, max: N) -> Schema<N, I, E>
where
  N: NumericBound,
  E: EffectData + 'static,
  I: 'static,
{
  constrain(
    s,
    move |v| *v < max,
    format!("must be < {max}"),
    |c| {
      tighten_max(&mut c.exclusive_maximum, max.to_f64());
    },
  )
}

/// `min <= value <= max` (JSON Schema `minimum` + `maximum`).
pub fn range<N, I, E>(s: Schema<N, I, E>, min: N, max: N) -> Schema<N, I, E>
where
  N: NumericBound,
  E: EffectData + 'static,
  I: 'static,
{
  maximum(minimum(s, min), max)
}

/// At least `n` elements (JSON Schema `minItems`).
pub fn min_items<A, I, E>(s: Schema<Vec<A>, I, E>, n: usize) -> Schema<Vec<A>, I, E>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  constrain(
    s,
    move |v| v.len() >= n,
    format!("must have at least {n} items"),
    |c| tighten_min(&mut c.min_items, n),
  )
}

/// At most `n` elements (JSON Schema `maxItems`).
pub fn max_items<A, I, E>(s: Schema<Vec<A>, I, E>, n: usize) -> Schema<Vec<A>, I, E>
where
  E: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  constrain(
    s,
    move |v| v.len() <= n,
    format!("must have at most {n} items"),
    |c| tighten_max(&mut c.max_items, n),
  )
}

fn is_email(v: &str) -> bool {
  let Some((local, domain)) = v.split_once('@') else {
    return false;
  };
  let local_ok = !local.is_empty()
    && local
      .chars()
      .all(|ch| !ch.is_whitespace() && !ch.is_control() && ch != '@');
  let labels: Vec<&str> = domain.split('.').collect();
  let domain_ok = labels.len() >= 2
    && labels.iter().all(|l| {
      !l.is_empty()
        && !l.starts_with('-')
        && !l.ends_with('-')
        && l.chars().all(|ch| ch.is_alphanumeric() || ch == '-')
    });
  local_ok && domain_ok
}

fn is_uuid(v: &str) -> bool {
  v.len() == 36
    && v.char_indices().all(|(i, ch)| match i {
      8 | 13 | 18 | 23 => ch == '-',
      _ => ch.is_ascii_hexdigit(),
    })
}

fn is_url(v: &str) -> bool {
  let Some((scheme, rest)) = v.split_once("://") else {
    return false;
  };
  let mut sc = scheme.chars();
  let scheme_ok = sc.next().is_some_and(|ch| ch.is_ascii_alphabetic())
    && sc.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'));
  let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
  scheme_ok
    && !authority.is_empty()
    && rest
      .chars()
      .all(|ch| !ch.is_whitespace() && !ch.is_control())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::schema::parse::{array, f64, i64, string};

  fn s() -> Schema<String, String, ()> {
    string::<()>()
  }

  #[test]
  fn length_validators_check_and_record() {
    let v = length(s(), 2, 4);
    assert!(v.decode("a".into()).is_err());
    assert!(v.decode("abcde".into()).is_err());
    assert_eq!(v.decode("äbc".into()).unwrap(), "äbc");
    assert_eq!(v.ast().constraints.min_length, Some(2));
    assert_eq!(v.ast().constraints.max_length, Some(4));
    let e = non_empty(s()).decode(String::new()).unwrap_err();
    assert_eq!(e.message, "must not be empty");
  }

  #[test]
  fn stacked_bounds_keep_the_tighter_one() {
    let v = min_length(min_length(s(), 5), 3);
    assert_eq!(v.ast().constraints.min_length, Some(5));
    let r = range(range(i64::<()>(), 0, 100), 10, 200);
    assert_eq!(r.ast().constraints.minimum, Some(10.0));
    assert_eq!(r.ast().constraints.maximum, Some(100.0));
  }

  #[test]
  fn email_accepts_and_rejects() {
    let v = email(s());
    for ok in ["a@b.co", "first.last+tag@sub.example.org"] {
      assert!(v.decode(ok.into()).is_ok(), "{ok}");
    }
    for bad in [
      "", "a", "@b.co", "a@", "a@b", "a@b..co", "a b@c.de", "a@-b.co", "a@b@c.de",
    ] {
      assert!(v.decode(bad.into()).is_err(), "{bad}");
    }
    assert_eq!(v.ast().constraints.format.as_deref(), Some("email"));
  }

  #[test]
  fn uuid_accepts_and_rejects() {
    let v = uuid(s());
    assert!(
      v.decode("123e4567-E89B-12d3-a456-426614174000".into())
        .is_ok()
    );
    assert!(v.decode("123e4567e89b12d3a456426614174000".into()).is_err());
    assert!(
      v.decode("123e4567-e89b-12d3-a456-42661417400g".into())
        .is_err()
    );
    assert_eq!(v.ast().constraints.format.as_deref(), Some("uuid"));
  }

  #[test]
  fn url_accepts_and_rejects() {
    let v = url(s());
    for ok in [
      "https://example.com",
      "http://localhost:8080/a?b=c#d",
      "svn+ssh://h/x",
    ] {
      assert!(v.decode(ok.into()).is_ok(), "{ok}");
    }
    for bad in [
      "example.com",
      "https://",
      "https:///path",
      "1http://x",
      "https://a b",
    ] {
      assert!(v.decode(bad.into()).is_err(), "{bad}");
    }
    assert_eq!(v.ast().constraints.format.as_deref(), Some("uri"));
  }

  #[test]
  fn pattern_uses_matcher_and_records_source() {
    let v = pattern(s(), "^[a-z]+$", |t| {
      !t.is_empty() && t.chars().all(|c| c.is_ascii_lowercase())
    });
    assert!(v.decode("abc".into()).is_ok());
    let e = v.decode("aB".into()).unwrap_err();
    assert_eq!(e.message, "must match pattern ^[a-z]+$");
    assert_eq!(v.ast().constraints.pattern.as_deref(), Some("^[a-z]+$"));
  }

  #[test]
  fn one_of_checks_membership_and_records_enum() {
    let v = one_of(s(), &["red", "green"]);
    assert!(v.decode("red".into()).is_ok());
    assert_eq!(
      v.decode("blue".into()).unwrap_err().message,
      "must be one of: red, green"
    );
    assert_eq!(
      v.ast().constraints.enum_values,
      Some(vec![
        Unknown::String("red".into()),
        Unknown::String("green".into())
      ])
    );
  }

  #[test]
  fn numeric_bounds() {
    let n = range(i64::<()>(), 1, 10);
    assert!(n.decode(0).is_err());
    assert!(n.decode(11).is_err());
    assert_eq!(n.decode(10).unwrap(), 10);
    assert_eq!(n.decode(0).unwrap_err().message, "must be >= 1");
    let x = exclusive_maximum(exclusive_minimum(f64::<()>(), 0.0), 1.0);
    assert!(x.decode(0.0).is_err());
    assert!(x.decode(1.0).is_err());
    assert_eq!(x.decode(0.5).unwrap(), 0.5);
    assert_eq!(x.ast().constraints.exclusive_minimum, Some(0.0));
    assert_eq!(x.ast().constraints.exclusive_maximum, Some(1.0));
  }

  #[test]
  fn item_counts() {
    let a = max_items(min_items(array(i64::<()>()), 1), 2);
    assert!(a.decode(vec![]).is_err());
    assert!(a.decode(vec![1, 2, 3]).is_err());
    assert_eq!(a.decode(vec![1]).unwrap(), vec![1]);
    assert_eq!(a.ast().constraints.min_items, Some(1));
    assert_eq!(a.ast().constraints.max_items, Some(2));
  }

  #[test]
  fn validators_apply_on_decode_unknown_too() {
    let v = email(s());
    assert!(v.decode_unknown(&Unknown::String("x".into())).is_err());
    assert!(v.decode_unknown_all(&Unknown::String("x".into())).is_err());
    assert!(v.decode_unknown(&Unknown::String("x@y.io".into())).is_ok());
  }
}
