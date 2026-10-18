// M16: `query` / `execute` take `impl Into<Sql>`, and only a `&'static str` converts, so SQL
// built at run time (`format!`, a `String` parameter) does not compile.
use id_effect::Effect;
use id_effect_sql::{SqlClientEnv, SqlError, SqlRow, execute, query};

fn by_table(table: &str) -> Effect<Vec<SqlRow>, SqlError, SqlClientEnv> {
  query(format!("SELECT * FROM {table}"), vec![])
}

fn run(sql: String) -> Effect<u64, SqlError, SqlClientEnv> {
  execute(sql, vec![])
}

fn main() {
  let _ = by_table("users");
  let _ = run(String::from("DELETE FROM users"));
}
