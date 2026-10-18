// `#[derive(Schema)]` with a field type it has no codec for: one error, at the field's type
// (the decode, decode-all and encode codegen used to report it three times).
use id_effect::Schema;

#[derive(Schema)]
pub struct Pair {
  pub ok: i64,
  pub pair: (i64, i64),
}

fn main() {}
