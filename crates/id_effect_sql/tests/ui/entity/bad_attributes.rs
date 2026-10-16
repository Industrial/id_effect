use id_effect_sql::{Entity, FromRow};

#[derive(FromRow, Entity)]
#[entity(name = "accounts")]
struct UnknownKey {
  #[entity(id)]
  id: i64,
}

#[derive(FromRow, Entity)]
struct GeneratedWithoutId {
  #[entity(id)]
  id: i64,
  #[entity(generated)]
  serial: i64,
}

#[derive(FromRow, Entity)]
struct TwoVersions {
  #[entity(id)]
  id: i64,
  #[entity(version)]
  a: i64,
  #[entity(version)]
  b: i64,
}

#[derive(FromRow)]
struct Audit {
  by: String,
}

#[derive(FromRow, Entity)]
struct Flattened {
  #[entity(id)]
  id: i64,
  #[from_row(flatten)]
  audit: Audit,
}

fn main() {}
