use id_effect_parse::codec::quoted_string;

#[test]
fn quoted_string_round_trips() {
  let codec = quoted_string();
  let wire = codec.print(&"hello\n".to_string());
  let (parsed, rest) = codec.parse(wire).unwrap();
  assert_eq!(parsed, "hello\n");
  assert!(rest.is_empty());
}

#[test]
fn quoted_string_parses_escapes() {
  let codec = quoted_string();
  let (parsed, _) = codec.parse("\"a\\tb\"".to_string()).unwrap();
  assert_eq!(parsed, "a\tb");
}

#[test]
fn quoted_string_rejects_unquoted_input() {
  let codec = quoted_string();
  assert!(codec.parse("hello".to_string()).is_err());
}

#[test]
fn quoted_string_prints_escapes() {
  let codec = quoted_string();
  let wire = codec.print(&"a\t\"b".to_string());
  assert_eq!(wire, "\"a\\t\\\"b\"");
}

#[test]
fn codec_map_round_trips() {
  let base = quoted_string();
  let mapped = base.map(|s| s.len(), |n| "x".repeat(*n));
  let wire = mapped.print(&3);
  let (parsed, rest) = mapped.parse(wire).unwrap();
  assert_eq!(parsed, 3);
  assert!(rest.is_empty());
}
