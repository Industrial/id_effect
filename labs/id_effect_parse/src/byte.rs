//! Byte-oriented parser combinators over [`Vec<u8>`] input.

use crate::parser::{ParseFailure, Parsed, Parser};

/// Parse a single expected byte.
#[must_use]
pub fn byte(expected: u8) -> Parser<Vec<u8>, u8, ParseFailure> {
  Parser::new(move |input: Vec<u8>| match input.first() {
    Some(found) if *found == expected => Ok((expected, input[1..].to_vec())),
    Some(found) => Err(ParseFailure::new(format!(
      "expected byte {expected}, found {found}"
    ))),
    None => Err(ParseFailure::new(format!(
      "expected byte {expected}, found end of input"
    ))),
  })
}

/// Parse an exact byte prefix.
#[must_use]
pub fn byte_tag(literal: &'static [u8]) -> Parser<Vec<u8>, Vec<u8>, ParseFailure> {
  Parser::new(move |input: Vec<u8>| {
    if input.starts_with(literal) {
      Ok((literal.to_vec(), input[literal.len()..].to_vec()))
    } else {
      Err(ParseFailure::new(format!(
        "expected byte tag {:?}",
        String::from_utf8_lossy(literal)
      )))
    }
  })
}

/// Parse signed ASCII digits into `i64`.
#[must_use]
pub fn byte_int() -> Parser<Vec<u8>, i64, ParseFailure> {
  Parser::new(|input: Vec<u8>| {
    let mut sign = 1_i64;
    let mut idx = 0usize;
    if input.first() == Some(&b'-') {
      sign = -1;
      idx = 1;
    } else if input.first() == Some(&b'+') {
      idx = 1;
    }
    let start = idx;
    while idx < input.len() && input[idx].is_ascii_digit() {
      idx += 1;
    }
    if idx == start {
      return Err(ParseFailure::new("expected integer"));
    }
    let digits = std::str::from_utf8(&input[start..idx])
      .map_err(|_| ParseFailure::new("invalid utf8 in integer"))?;
    let value = digits
      .parse::<i64>()
      .map_err(|err| ParseFailure::new(format!("invalid integer: {err}")))?;
    Ok((sign * value, input[idx..].to_vec()))
  })
}

/// Skip ASCII whitespace bytes.
#[must_use]
pub fn byte_ws() -> Parser<Vec<u8>, (), ParseFailure> {
  Parser::new(|input: Vec<u8>| {
    let mut len = 0usize;
    for (idx, b) in input.iter().enumerate() {
      if b.is_ascii_whitespace() {
        len = idx + 1;
      } else {
        break;
      }
    }
    Ok(((), input[len..].to_vec()))
  })
}

/// Run a byte parser on a slice, copying into an owned buffer first.
pub fn parse_bytes<O, E>(
  parser: &Parser<Vec<u8>, O, E>,
  input: &[u8],
) -> Result<Parsed<Vec<u8>, O>, E>
where
  O: Send + Sync + 'static,
  E: Send + Sync + 'static,
{
  parser.parse(input.to_vec())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn byte_tag_and_int() {
    let parser = byte_tag(b"N").and_then(|_| byte_int());
    let (value, rest) = parse_bytes(&parser, b"N42rest").unwrap();
    assert_eq!(value, 42);
    assert_eq!(rest, b"rest");
  }
}
