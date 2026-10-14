use id_effect_parse::{ParseFailure, char, int, parse_all, parse_str, tag, ws};

#[test]
fn char_parses_expected_character() {
  let (found, rest) = parse_str(&char('x'), "xyz").unwrap();
  assert_eq!(found, 'x');
  assert_eq!(rest, "yz");
}

#[test]
fn tag_parses_literal_prefix() {
  let (matched, rest) = parse_str(&tag("hello"), "hello world").unwrap();
  assert_eq!(matched, "hello");
  assert_eq!(rest, " world");
}

#[test]
fn int_parses_digits() {
  let (value, rest) = parse_str(&int(), "42px").unwrap();
  assert_eq!(value, 42);
  assert_eq!(rest, "px");
}

#[test]
fn map_transforms_output() {
  let parser = int().map(|n| n * 2);
  let (value, _) = parse_str(&parser, "21").unwrap();
  assert_eq!(value, 42);
}

#[test]
fn and_then_sequences_parsers() {
  let parser = tag("(")
    .and_then(|_| int())
    .and_then(|n| tag(")").map(move |_| n));
  let value = parse_all(&parser, "(7)".to_string()).unwrap();
  assert_eq!(value, 7);
}

#[test]
fn alt_tries_second_parser() {
  let parser = tag("foo").alt(tag("bar"));
  let (matched, _) = parse_str(&parser, "bar").unwrap();
  assert_eq!(matched, "bar");
}

#[test]
fn many_collects_zero_or_more() {
  let parser = char('a').many();
  let (values, rest) = parse_str(&parser, "aaab").unwrap();
  assert_eq!(values, vec!['a', 'a', 'a']);
  assert_eq!(rest, "b");
}

#[test]
fn ws_skips_whitespace() {
  let ((), rest) = parse_str(&ws(), "  \t rest").unwrap();
  assert_eq!(rest, "rest");
}

#[test]
fn parse_reports_failure() {
  let err = parse_str(&char('z'), "abc").unwrap_err();
  assert_eq!(err, ParseFailure::new("expected 'z', found 'a'"));
}
