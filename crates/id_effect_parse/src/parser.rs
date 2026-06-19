//! Parser combinator core — [`Parser`] with `map`, `and_then`, `alt`, and `many`.

use core::fmt;
use std::sync::Arc;

/// Output and unconsumed input from a successful parse.
pub type Parsed<I, O> = (O, I);

/// A parse failure with a human-readable message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseFailure {
  /// What went wrong.
  pub message: String,
}

impl ParseFailure {
  /// Construct a failure from a static or owned message.
  #[must_use]
  pub fn new(message: impl Into<String>) -> Self {
    Self {
      message: message.into(),
    }
  }
}

impl fmt::Display for ParseFailure {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.message)
  }
}

impl std::error::Error for ParseFailure {}

type ParseFn<I, O, E> = Arc<dyn Fn(I) -> Result<Parsed<I, O>, E> + Send + Sync>;

/// A reusable parser from input `I` to output `O` with error `E`.
#[derive(Clone)]
pub struct Parser<I, O, E> {
  run: ParseFn<I, O, E>,
}

impl<I, O, E> Parser<I, O, E>
where
  I: Send + Sync + 'static,
  O: Send + Sync + 'static,
  E: Send + Sync + 'static,
{
  /// Wrap a parsing function.
  #[must_use]
  pub fn new(run: impl Fn(I) -> Result<Parsed<I, O>, E> + Send + Sync + 'static) -> Self {
    Self { run: Arc::new(run) }
  }

  /// Run this parser on `input`.
  pub fn parse(&self, input: I) -> Result<Parsed<I, O>, E> {
    (self.run)(input)
  }

  /// Map the parsed output with `f`.
  #[must_use]
  pub fn map<U>(self, f: impl Fn(O) -> U + Send + Sync + Clone + 'static) -> Parser<I, U, E>
  where
    U: Send + Sync + 'static,
  {
    let inner = self;
    Parser::new(move |input| inner.parse(input).map(|(o, rest)| (f(o), rest)))
  }

  /// Sequence with a parser chosen from the parsed output.
  #[must_use]
  pub fn and_then<U>(
    self,
    f: impl Fn(O) -> Parser<I, U, E> + Send + Sync + Clone + 'static,
  ) -> Parser<I, U, E>
  where
    U: Send + Sync + 'static,
  {
    let inner = self;
    Parser::new(move |input| {
      let (o, rest) = inner.parse(input)?;
      f(o).parse(rest)
    })
  }

  /// Try `self`, then `other` when the first parser fails.
  #[must_use]
  pub fn alt(self, other: Self) -> Self
  where
    I: Clone,
    E: Clone,
  {
    let first = self;
    Parser::new(move |input: I| match first.parse(input.clone()) {
      Ok(parsed) => Ok(parsed),
      Err(first_err) => other.parse(input).map_err(|_| first_err),
    })
  }

  /// Parse zero or more occurrences; stops at the first failure without consuming.
  #[must_use]
  pub fn many(self) -> Parser<I, Vec<O>, E>
  where
    I: Clone,
  {
    let inner = self;
    Parser::new(move |mut input: I| {
      let mut out = Vec::new();
      loop {
        let checkpoint = input.clone();
        match inner.parse(input) {
          Ok((item, rest)) => {
            out.push(item);
            input = rest;
          }
          Err(_) => return Ok((out, checkpoint)),
        }
      }
    })
  }
}

/// Parse a string slice by copying into an owned buffer first.
pub fn parse_str<O, E>(parser: &Parser<String, O, E>, input: &str) -> Result<Parsed<String, O>, E>
where
  O: Send + Sync + 'static,
  E: Send + Sync + 'static,
{
  parser.parse(input.to_string())
}

/// Parse a single expected character from a string buffer.
#[must_use]
pub fn char(expected: char) -> Parser<String, char, ParseFailure> {
  Parser::new(move |mut input: String| {
    let mut chars = input.chars();
    match chars.next() {
      Some(found) if found == expected => {
        let len = found.len_utf8();
        let rest = input.split_off(len);
        Ok((found, rest))
      }
      Some(found) => Err(ParseFailure::new(format!(
        "expected '{expected}', found '{found}'"
      ))),
      None => Err(ParseFailure::new(format!(
        "expected '{expected}', found end of input"
      ))),
    }
  })
}

/// Parse an exact literal prefix.
#[must_use]
pub fn tag(literal: &'static str) -> Parser<String, String, ParseFailure> {
  Parser::new(move |input: String| {
    if let Some(rest) = input.strip_prefix(literal) {
      Ok((literal.to_string(), rest.to_string()))
    } else {
      Err(ParseFailure::new(format!("expected '{literal}'")))
    }
  })
}

/// Parse a non-empty run of ASCII digits into `i64`.
#[must_use]
pub fn int() -> Parser<String, i64, ParseFailure> {
  Parser::new(|input: String| {
    let mut len = 0usize;
    for (idx, ch) in input.char_indices() {
      if ch.is_ascii_digit() {
        len = idx + ch.len_utf8();
      } else {
        break;
      }
    }
    if len == 0 {
      return Err(ParseFailure::new("expected integer"));
    }
    let (digits, tail) = input.split_at(len);
    let value = digits
      .parse::<i64>()
      .map_err(|err| ParseFailure::new(format!("invalid integer: {err}")))?;
    Ok((value, tail.to_string()))
  })
}

/// Skip ASCII whitespace.
#[must_use]
pub fn ws() -> Parser<String, (), ParseFailure> {
  Parser::new(|input: String| {
    let mut len = 0usize;
    for (idx, ch) in input.char_indices() {
      if ch.is_ascii_whitespace() {
        len = idx + ch.len_utf8();
      } else {
        break;
      }
    }
    Ok(((), input[len..].to_string()))
  })
}

/// Convenience: parse and return only the output (ignoring leftover input).
pub fn parse_all<I, O, E>(parser: &Parser<I, O, E>, input: I) -> Result<O, E>
where
  I: Send + Sync + 'static,
  O: Send + Sync + 'static,
  E: Send + Sync + 'static,
{
  parser.parse(input).map(|(out, _rest)| out)
}
