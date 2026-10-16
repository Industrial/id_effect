use id_effect_sql::{Entity, FromRow};

#[derive(FromRow, Entity)]
struct NoKey {
  name: String,
}

fn main() {}
