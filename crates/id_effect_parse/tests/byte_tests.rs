use id_effect_parse::{byte_int, byte_tag, parse_bytes};

#[test]
fn byte_parsers_work_on_slices() {
  let parser = byte_tag(b"ID").and_then(|_| byte_int());
  let (id, rest) = parse_bytes(&parser, b"ID99tail").unwrap();
  assert_eq!(id, 99);
  assert_eq!(rest, b"tail");
}
