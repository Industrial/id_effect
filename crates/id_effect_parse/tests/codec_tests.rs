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
