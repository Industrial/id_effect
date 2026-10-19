// ADR 0013 R5 (M16): `query` takes SQL text known at compile time; a `String` built at run time
// (here with user input spliced in) does not convert to `Sql`.
use id_effect_kit::sql::{SqlClientEnv, query};

fn main() {
  let name = std::env::args().nth(1).unwrap_or_default();
  let sql = format!("SELECT id FROM users WHERE name = '{name}'");
  let _ = query::<SqlClientEnv, _>(sql, vec![]);
}
