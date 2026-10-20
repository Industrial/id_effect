// M-01: non-literal SQL into the `&str` methods of `SqlClient` / `SqlTransaction` (0.11.0 D9).
// The MachineApplicable case (a wrapped literal) is `raw_sql_non_literal_fix.rs`.
// Mirrors id_effect_sql's shapes: `query`/`execute(&self, sql: &str, params: &[SqlValue])`.
#![allow(dead_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);
struct SqlValue;
struct SqlRow;
struct SqlError;

trait SqlClient {
  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError, ()>;
  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError, ()>;
}

trait SqlTransaction {
  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError, ()>;
}

/// `id_effect_sql::Sql`: only a `&'static str` converts into one.
struct Sql(&'static str);

impl Sql {
  fn as_str(&self) -> &str {
    self.0
  }
}

const BY_ID: &str = "SELECT id FROM users WHERE id = $1";
static COUNT: &str = "SELECT count(*) FROM users";

fn literals(c: &dyn SqlClient, tx: &dyn SqlTransaction) {
  let _ = c.query("SELECT 1", &[]);
  let _ = c.query(BY_ID, &[SqlValue]);
  let _ = c.query(COUNT, &[]);
  let bound = "DELETE FROM sessions WHERE expired";
  let _ = c.execute(bound, &[]);
  let typed = Sql("SELECT id FROM users");
  let _ = c.query(typed.as_str(), &[]);
  let _ = tx.execute(concat!("UPDATE t ", "SET a = 1"), &[]);
  let _ = SqlClient::query(c, "SELECT 2", &[]);
}

// A `&'static str` parameter holds every caller to a literal or a `const`.
fn run_static(c: &dyn SqlClient, sql: &'static str) {
  let _ = c.execute(sql, &[]);
}

fn built_at_run_time(c: &dyn SqlClient, tx: &dyn SqlTransaction, table: &str, sql: String) {
  let _ = c.query(&format!("SELECT * FROM {table}"), &[]);
  let _ = c.execute(&sql, &[]);
  let _ = tx.execute(table, &[]);
  let mut reassigned = "SELECT 1";
  reassigned = table;
  let _ = c.query(reassigned, &[]);
  let _ = SqlClient::execute(c, &sql, &[]);
}

/// A wrapper that forwards its own `sql` is the boundary: its callers are linted.
struct Logged<C>(C);

impl<C: SqlClient> SqlClient for Logged<C> {
  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError, ()> {
    self.0.query(sql, params)
  }
  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError, ()> {
    self.0.execute(sql, params)
  }
}

fn main() {}
