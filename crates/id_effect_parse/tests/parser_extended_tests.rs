use id_effect_parse::{
  ParseFailure, between, bool_lit, char, float, int, many1, optional, parse_str, sep_by,
  signed_int, void,
};

#[test]
fn signed_int_parses_negative() {
  let (value, rest) = parse_str(&signed_int(), "-42x").unwrap();
  assert_eq!(value, -42);
  assert_eq!(rest, "x");
}

#[test]
fn bool_and_float_literals() {
  let (b, _) = parse_str(&bool_lit(), "true rest").unwrap();
  assert!(b);
  let (f, _) = parse_str(&float(), "-1.5end").unwrap();
  assert!((f + 1.5).abs() < f64::EPSILON);
}

#[test]
fn optional_many1_sep_by_between() {
  let opt = optional(int());
  assert_eq!(parse_str(&opt, "9").unwrap().0, Some(9));
  assert_eq!(parse_str(&opt, "x").unwrap().0, None);

  let digits = many1(char('a'));
  assert_eq!(parse_str(&digits, "aaa").unwrap().0, vec!['a', 'a', 'a']);

  let csv = sep_by(int(), void(char(',')));
  assert_eq!(parse_str(&csv, "1,2,3").unwrap().0, vec![1, 2, 3]);

  let grouped = between(void(char('(')), void(char(')')), int());
  assert_eq!(parse_str(&grouped, "(7)").unwrap().0, 7);
}

#[test]
fn int_alias_still_works() {
  let (value, _) = parse_str(&int(), "12").unwrap();
  assert_eq!(value, 12);
  assert!(parse_str(&int(), "nope").is_err());
  let err = parse_str(&int(), "nope").unwrap_err();
  assert!(matches!(err, ParseFailure { .. }));
}
