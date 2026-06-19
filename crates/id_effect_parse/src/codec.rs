//! Invertible codecs — paired parse and print.

use crate::parser::{ParseFailure, Parser};

/// A codec pairs a parser with a printer for round-trippable formats.
#[derive(Clone)]
pub struct Codec<I, O, E> {
  parser: Parser<I, O, E>,
  print: std::sync::Arc<dyn Fn(&O) -> I + Send + Sync>,
}

impl<I, O, E> Codec<I, O, E>
where
  I: Send + Sync + 'static,
  O: Send + Sync + 'static,
  E: Send + Sync + 'static,
{
  /// Build a codec from parse and print functions.
  #[must_use]
  pub fn new(parser: Parser<I, O, E>, print: impl Fn(&O) -> I + Send + Sync + 'static) -> Self {
    Self {
      parser,
      print: std::sync::Arc::new(print),
    }
  }

  /// Access the underlying parser.
  #[must_use]
  pub fn parser(&self) -> &Parser<I, O, E> {
    &self.parser
  }

  /// Parse input into a value and leftover buffer.
  pub fn parse(&self, input: I) -> Result<(O, I), E> {
    self.parser.parse(input)
  }

  /// Print a value back to the wire/input representation.
  pub fn print(&self, value: &O) -> I {
    (self.print)(value)
  }

  /// Map the parsed/printed type.
  #[must_use]
  pub fn map<U>(
    self,
    to: impl Fn(O) -> U + Send + Sync + Clone + 'static,
    from: impl Fn(&U) -> O + Send + Sync + Clone + 'static,
  ) -> Codec<I, U, E>
  where
    U: Send + Sync + 'static,
  {
    let print = self.print.clone();
    Codec {
      parser: self.parser.map(to),
      print: std::sync::Arc::new(move |value| print(&(from)(value))),
    }
  }
}

/// Parse and print quoted string literals.
#[must_use]
pub fn quoted_string() -> Codec<String, String, ParseFailure> {
  let parser = Parser::new(|input: String| {
    if !input.starts_with('"') {
      return Err(ParseFailure::new("expected opening quote"));
    }
    let mut out = String::new();
    let mut escaped = false;
    for (idx, ch) in input.char_indices().skip(1) {
      if escaped {
        match ch {
          'n' => out.push('\n'),
          't' => out.push('\t'),
          '\\' => out.push('\\'),
          '"' => out.push('"'),
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
        return Ok((out, input[end..].to_string()));
      }
      out.push(ch);
    }
    Err(ParseFailure::new("unterminated quoted string"))
  });

  Codec::new(parser, |value| format!("\"{}\"", escape(value)))
}

fn escape(value: &str) -> String {
  value
    .replace('\\', "\\\\")
    .replace('"', "\\\"")
    .replace('\n', "\\n")
    .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn codec_parser_accessor() {
    let codec = quoted_string();
    let (text, _) = codec.parser().parse("\"z\"".into()).expect("parse");
    assert_eq!(text, "z");
  }

  #[test]
  fn codec_map_round_trip() {
    let codec = quoted_string().map(|s| s.len(), |n| "x".repeat(*n));
    let (len, _) = codec.parse("\"ab\"".into()).expect("parse");
    assert_eq!(len, 2);
    assert_eq!(codec.print(&2), "\"xx\"");
  }

  #[test]
  fn rejects_unquoted_input() {
    assert!(quoted_string().parse("nope".to_string()).is_err());
  }

  #[test]
  fn rejects_unterminated_string() {
    assert!(quoted_string().parse("\"open".to_string()).is_err());
  }

  #[test]
  fn parses_escaped_tab() {
    let (text, _) = quoted_string().parse("\"a\tb\"".into()).expect("parse");
    assert_eq!(text, "a	b");
  }

  #[test]
  fn parses_escaped_newline() {
    let (text, _) = quoted_string().parse("\"a\\nb\"".into()).expect("parse");
    assert_eq!(text, "a\nb");
  }

  #[test]
  fn print_escapes_special_characters() {
    let wire = quoted_string().print(&"line\tquote\"".to_string());
    assert!(wire.contains("\\t"));
    assert!(wire.contains("\\\""));
  }
}
