use id_effect_sql::{Entity, FromRow};

#[derive(FromRow, Entity)]
struct Tagged {
  #[entity(id)]
  id: i64,
  #[entity(version)]
  version: String,
}

fn main() {}
