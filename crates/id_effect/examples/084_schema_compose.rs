//! Ex 084 — `struct_` composes field schemas with path-aware errors.
use id_effect::EffectData;
use id_effect::schema::{Unknown, i64, string, struct_};

#[derive(Clone, Debug, EffectData)]
struct RowTag;

fn main() {
  let s = struct_("a", i64::<RowTag>(), "b", string::<RowTag>());
  let obj = Unknown::Object(
    [
      ("a".to_owned(), Unknown::I64(1)),
      ("b".to_owned(), Unknown::String("x".to_owned())),
    ]
    .into_iter()
    .collect(),
  );
  let row = s.decode_unknown(&obj).expect("decode");
  assert_eq!(row, (1_i64, "x".to_owned()));
  println!("084_schema_compose ok");
}
