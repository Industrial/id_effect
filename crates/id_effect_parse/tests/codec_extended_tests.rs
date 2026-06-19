use id_effect_parse::{bool_codec, float_codec, int_codec, list, pair, quoted_string};

#[test]
fn codec_combinators_round_trip() {
  let c = pair(int_codec(), bool_codec());
  let wire = c.print(&(7, true));
  let (value, _) = c.parse(wire).unwrap();
  assert_eq!(value, (7, true));

  let list_c = list(int_codec());
  let wire = list_c.print(&vec![1, 2, 3]);
  let (values, _) = list_c.parse(wire).unwrap();
  assert_eq!(values, vec![1, 2, 3]);

  let s = quoted_string();
  assert_eq!(s.print(&"x".to_string()), "\"x\"");
  let f = float_codec();
  let (v, _) = f.parse("1.25".into()).unwrap();
  assert!((v - 1.25).abs() < f64::EPSILON);
}
