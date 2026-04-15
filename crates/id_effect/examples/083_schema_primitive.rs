//! Ex 083 — Primitive `Schema` codecs (`i64`).
use id_effect::EffectData;
use id_effect::schema::{Unknown, i64};

#[derive(Clone, Debug, EffectData)]
struct Tag;

fn main() {
  let s = i64::<Tag>();
  assert_eq!(s.decode(42_i64), Ok(42));
  assert_eq!(s.decode_unknown(&Unknown::I64(7)), Ok(7));
  println!("083_schema_primitive ok");
}
