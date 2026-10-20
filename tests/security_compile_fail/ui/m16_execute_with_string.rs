// ADR 0013 R5 (M16): `execute` takes SQL text known at compile time; a `String` built at run
// time does not convert to `Sql`.
use id_effect_kit::sql::{SqlClientEnv, execute};

fn main() {
  let table = std::env::args().nth(1).unwrap_or_default();
  let sql = "DELETE FROM ".to_string() + &table;
  let _ = execute::<SqlClientEnv, _>(sql, vec![]);
}
