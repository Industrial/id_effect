// M16: `query_raw` needs `RawSqlCapability` in the environment; the client alone is not enough.
use id_effect::Effect;
use id_effect_sql::{RawSql, SqlClientEnv, SqlError, SqlRow, query_raw};

fn by_table(table: &str) -> Effect<Vec<SqlRow>, SqlError, SqlClientEnv> {
  query_raw(RawSql::new(format!("SELECT * FROM {table}")), vec![])
}

fn main() {
  let _ = by_table("users");
}
