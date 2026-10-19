// ADR 0013 R5 (M16): SQL built at run time runs only with the `RawSqlCapability` in the
// environment; the client alone is not enough.
use id_effect_kit::sql::{RawSql, SqlClientEnv, query_raw};

fn main() {
  let table = std::env::args().nth(1).unwrap_or_default();
  let raw = RawSql::new(format!("SELECT count(*) FROM {table}"));
  let _ = query_raw::<SqlClientEnv, _, _>(raw, vec![]);
}
