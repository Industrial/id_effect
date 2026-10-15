//! Domain code over the `SqlClientService` capability, run against the in-memory
//! `TestSqlClient`: the client layer connects and closes it, and a transaction layer commits on
//! success and rolls back on failure.
//!
//! Run: `cargo run -p id_effect_sql --example 010_test_client_transaction`

use id_effect::{Cause, Effect, Exit, fail, need};
use id_effect_sql::{
  SqlClientEnv, SqlError, SqlParam, SqlRow, SqlTransactionEnv, SqlTransactionService,
  TestSqlClient, query, sql_client_layer, transaction_layer,
};

const ACTIVE: &str = "SELECT id FROM users WHERE active = $1";
const DEACTIVATE: &str = "UPDATE users SET active = false";

/// Reads the client from the environment.
fn count_active() -> Effect<usize, SqlError, SqlClientEnv> {
  query(ACTIVE, vec![SqlParam::Bool(true)]).map(|rows| rows.len())
}

/// Runs on the transaction's connection.
fn deactivate_all() -> Effect<u64, SqlError, SqlTransactionEnv> {
  need::<SqlTransactionService>().flat_map(|tx| tx.execute(DEACTIVATE, &[]).narrow())
}

fn main() {
  let test = TestSqlClient::new();
  test.script_query(
    ACTIVE,
    vec![SqlRow::new(vec!["1".into()]), SqlRow::new(vec!["2".into()])],
  );
  test.script_execute(DEACTIVATE, 2);

  // Success: the statement is committed, then the client is closed.
  let program = count_active()
    .flat_map(|n| {
      deactivate_all()
        .provide_layer(transaction_layer())
        .map(move |changed| (n, changed))
    })
    .provide_layer(sql_client_layer::<SqlError>(test.clone()));
  assert!(matches!(program.block_on(), Exit::Success((2, 2))));
  assert_eq!((test.commits(), test.rollbacks(), test.closes()), (1, 0, 1));
  assert_eq!(test.recorded_executes().len(), 1);

  // Failure inside the transaction: it is rolled back and its statement discarded.
  let failing = deactivate_all()
    .flat_map(|_| fail::<u64, SqlError, SqlTransactionEnv>(SqlError::Unsupported("stop".into())))
    .provide_layer(transaction_layer())
    .provide_layer(sql_client_layer::<SqlError>(test.clone()));
  assert!(matches!(
    failing.block_on(),
    Exit::Failure(Cause::Fail(SqlError::Unsupported(ref m))) if m == "stop"
  ));
  assert_eq!((test.commits(), test.rollbacks(), test.closes()), (1, 1, 2));
  assert_eq!(test.recorded_executes().len(), 1);

  println!("010_test_client_transaction ok (2 active, 1 commit, 1 rollback)");
}
