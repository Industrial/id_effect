//! Minimal JSON parser producing [`id_effect::schema::Unknown`].

use id_effect::schema::{ParseError, Unknown};
use std::collections::BTreeMap;

/// Parse a JSON value from `input`, returning the value and unconsumed suffix.
pub fn parse_json_value(input: &str) -> Result<(Unknown, String), ParseError> {
  let trimmed = input.trim_start();
  let (value, rest) = parse_value(trimmed)?;
  Ok((value, rest.to_string()))
}

/// Parse a complete JSON document (no trailing non-whitespace).
pub fn parse_json_document(input: &str) -> Result<Unknown, ParseError> {
  let (value, rest) = parse_json_value(input)?;
  if rest.trim().is_empty() {
    Ok(value)
  } else {
    Err(ParseError::new("", "trailing input after JSON value"))
  }
}

fn parse_value(input: &str) -> Result<(Unknown, &str), ParseError> {
  let input = input.trim_start();
  if input.is_empty() {
    return Err(ParseError::new("", "unexpected end of input"));
  }
  match input.as_bytes()[0] {
    b'"' => parse_string(input),
    b'{' => parse_object(input),
    b'[' => parse_array(input),
    b't' | b'f' => parse_bool(input),
    b'n' => parse_null(input),
    b'-' | b'0'..=b'9' => parse_number(input),
    _ => Err(ParseError::new("", "expected JSON value")),
  }
}

fn parse_null(input: &str) -> Result<(Unknown, &str), ParseError> {
  input
    .strip_prefix("null")
    .map(|rest| (Unknown::Null, rest))
    .ok_or_else(|| ParseError::new("", "expected null"))
}

fn parse_bool(input: &str) -> Result<(Unknown, &str), ParseError> {
  if let Some(rest) = input.strip_prefix("true") {
    return Ok((Unknown::Bool(true), rest));
  }
  if let Some(rest) = input.strip_prefix("false") {
    return Ok((Unknown::Bool(false), rest));
  }
  Err(ParseError::new("", "expected boolean"))
}

fn parse_string(input: &str) -> Result<(Unknown, &str), ParseError> {
  if !input.starts_with('"') {
    return Err(ParseError::new("", "expected string"));
  }
  let mut out = String::new();
  let mut escaped = false;
  for (idx, ch) in input.char_indices().skip(1) {
    if escaped {
      match ch {
        '"' => out.push('"'),
        '\\' => out.push('\\'),
        '/' => out.push('/'),
        'b' => out.push('\x08'),
        'f' => out.push('\x0c'),
        'n' => out.push('\n'),
        'r' => out.push('\r'),
        't' => out.push('\t'),
        'u' => {
          let hex = input[idx..].chars().take(4).collect::<String>();
          if hex.len() != 4 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ParseError::new("", "invalid unicode escape"));
          }
          let code = u32::from_str_radix(&hex, 16)
            .map_err(|_| ParseError::new("", "invalid unicode escape"))?;
          let decoded =
            char::from_u32(code).ok_or_else(|| ParseError::new("", "invalid unicode scalar"))?;
          out.push(decoded);
        }
        other => out.push(other),
      }
      escaped = false;
      continue;
    }
    if ch == '\\' {
      escaped = true;
      continue;
    }
    if ch == '"' {
      let end = idx + ch.len_utf8();
      return Ok((Unknown::String(out), &input[end..]));
    }
    out.push(ch);
  }
  Err(ParseError::new("", "unterminated string"))
}

fn parse_number(input: &str) -> Result<(Unknown, &str), ParseError> {
  let mut end = 0usize;
  let bytes = input.as_bytes();
  if bytes.first() == Some(&b'-') {
    end = 1;
  }
  while end < bytes.len() && bytes[end].is_ascii_digit() {
    end += 1;
  }
  if end == 0 || (end == 1 && bytes[0] == b'-') {
    return Err(ParseError::new("", "expected number"));
  }
  if end < bytes.len() && bytes[end] == b'.' {
    end += 1;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
      end += 1;
    }
    let slice = &input[..end];
    let value = slice
      .parse::<f64>()
      .map_err(|_| ParseError::new("", "invalid number"))?;
    return Ok((Unknown::F64(value), &input[end..]));
  }
  let slice = &input[..end];
  let value = slice
    .parse::<i64>()
    .map_err(|_| ParseError::new("", "invalid integer"))?;
  Ok((Unknown::I64(value), &input[end..]))
}

fn parse_array(input: &str) -> Result<(Unknown, &str), ParseError> {
  let rest = input
    .strip_prefix('[')
    .ok_or_else(|| ParseError::new("", "expected array"))?;
  let mut items = Vec::new();
  let mut current = rest.trim_start();
  if let Some(rest) = current.strip_prefix(']') {
    return Ok((Unknown::Array(items), rest));
  }
  loop {
    let (item, after) = parse_value(current)?;
    items.push(item);
    current = after.trim_start();
    if let Some(rest) = current.strip_prefix(']') {
      return Ok((Unknown::Array(items), rest));
    }
    if !current.starts_with(',') {
      return Err(ParseError::new("", "expected ',' or ']' in array"));
    }
    current = current[1..].trim_start();
  }
}

fn parse_object(input: &str) -> Result<(Unknown, &str), ParseError> {
  let rest = input
    .strip_prefix('{')
    .ok_or_else(|| ParseError::new("", "expected object"))?;
  let mut map = BTreeMap::new();
  let mut current = rest.trim_start();
  if let Some(rest) = current.strip_prefix('}') {
    return Ok((Unknown::Object(map), rest));
  }
  loop {
    let (key, after_key) = parse_string(current)?;
    let Unknown::String(key) = key else {
      return Err(ParseError::new("", "expected object key string"));
    };
    current = after_key.trim_start();
    if !current.starts_with(':') {
      return Err(ParseError::new("", "expected ':' after object key"));
    }
    current = current[1..].trim_start();
    let (value, after_value) = parse_value(current)?;
    map.insert(key, value);
    current = after_value.trim_start();
    if let Some(rest) = current.strip_prefix('}') {
      return Ok((Unknown::Object(map), rest));
    }
    if !current.starts_with(',') {
      return Err(ParseError::new("", "expected ',' or '}' in object"));
    }
    current = current[1..].trim_start();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_scalars() {
    assert_eq!(parse_json_document("null").unwrap(), Unknown::Null);
    assert_eq!(parse_json_document("true").unwrap(), Unknown::Bool(true));
    assert_eq!(parse_json_document("42").unwrap(), Unknown::I64(42));
    assert_eq!(parse_json_document("-7").unwrap(), Unknown::I64(-7));
    assert_eq!(parse_json_document("1.5").unwrap(), Unknown::F64(1.5));
    assert_eq!(
      parse_json_document("\"hi\"").unwrap(),
      Unknown::String("hi".into())
    );
  }

  #[test]
  fn parses_object_and_array() {
    let doc = r#"{"name":"Ada","tags":["fp","rust"]}"#;
    let value = parse_json_document(doc).unwrap();
    assert!(matches!(value, Unknown::Object(_)));
  }

  #[test]
  fn rejects_trailing_garbage() {
    assert!(parse_json_document("42 trailing").is_err());
  }
}
