//! Ex 020 — per-test database isolation and fixtures, without Docker: a SQLite database file
//! with a committed schema, two "tests" that each insert the same account (a unique name) from
//! a seeded `Factory` inside `isolated_db()`, and the file left exactly as it was.
//!
//! Against PostgreSQL the same program runs over `Shared<Postgres>` (rollback mode), or over
//! `TemplateDb::isolated_db` for code that commits (clone mode) — see the book chapter
//! "Integration Testing".
//!
//! Run: `cargo run -p id_effect_test --example 020_isolated_db --features fixtures`

use id_effect::{Cause, Effect, Exit, caps};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient};
use id_effect_sql::{FromRow, SqlClientService, SqlError, SqlValue, execute, params, query_one_as};
use id_effect_test::fixtures::{BoxedStrategy, Strategy, from_schema};
use id_effect_test::{Factory, Fixture, isolated_db};
use id_effect_tokio::TokioRuntime;

/// An account row, decoded with `FromRow`.
#[derive(FromRow)]
struct Account {
  id: i64,
  name: String,
  balance: i64,
}

/// What a test inserts: schema-valid defaults, overridden per test.
#[derive(Clone, Debug)]
struct NewAccount {
  name: String,
  balance: i64,
}

impl Fixture for NewAccount {
  type Row = Account;

  fn arbitrary() -> BoxedStrategy<Self> {
    use id_effect::schema::{i64, refine, string};
    let name = from_schema(
      refine(string::<()>(), |s: &String| !s.is_empty(), "non-empty"),
      "[a-z]{3,10}",
    );
    let balance = from_schema(
      refine(i64::<()>(), |n| *n >= 0, "non-negative"),
      0..10_000i64,
    );
    (name, balance)
      .prop_map(|(name, balance)| NewAccount { name, balance })
      .boxed()
  }

  fn insert_sql(&self) -> (String, Vec<SqlValue>) {
    (
      "INSERT INTO accounts (name, balance) VALUES ($1, $2) RETURNING id, name, balance".into(),
      params![self.name.clone(), self.balance],
    )
  }
}

type Db = caps!(SqlClientService);

/// One "test": inserts `alice` (balance drawn from the seed) and counts what it sees.
fn a_test(seed: u64) -> Effect<(Account, i64), SqlError, Db> {
  let mut accounts = Factory::<NewAccount>::seeded(seed).with(|a| a.name = "alice".into());
  accounts.insert::<Db, _>().flat_map(|account| {
    query_one_as::<(i64,), Db, _>("SELECT COUNT(*) FROM accounts", vec![])
      .map(move |(n,)| (account, n))
  })
}

fn run<A: Send + 'static>(
  runtime: &TokioRuntime,
  eff: Effect<A, SqlError>,
) -> impl std::future::Future<Output = A> + '_ {
  let fut = runtime.run(eff);
  async move {
    match fut.await {
      Exit::Success(a) => a,
      Exit::Failure(Cause::Fail(e)) => panic!("isolated_db example failed: {e}"),
      Exit::Failure(cause) => panic!("isolated_db example failed: {cause:?}"),
    }
  }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
  let path = std::env::temp_dir().join(format!("id_effect_test_020_{}.db", std::process::id()));
  let client = || SqliteSqlClient::layer::<SqlError>(SqlitePoolConfig::file(&path));
  let runtime = TokioRuntime::current().expect("a tokio runtime");

  // The schema is committed once, as migrations would be.
  let schema: Effect<u64, SqlError, Db> = execute(
    "CREATE TABLE accounts (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, balance INTEGER NOT NULL)",
    vec![],
  );
  run(&runtime, schema.provide_layer(client())).await;

  // Two tests insert the same unique name: each sees only its own row.
  for seed in [1, 2] {
    let (account, seen) = run(
      &runtime,
      a_test(seed).provide_layer(client().provide_to(isolated_db())),
    )
    .await;
    println!(
      "test {seed}: inserted #{} {} (balance {}), sees {seen} account(s)",
      account.id, account.name, account.balance
    );
    assert_eq!((account.name.as_str(), seen), ("alice", 1));
  }

  // Both were rolled back.
  let left: Effect<(i64,), SqlError, Db> = query_one_as("SELECT COUNT(*) FROM accounts", vec![]);
  let (left,) = run(&runtime, left.provide_layer(client())).await;
  println!("committed accounts afterwards: {left}");
  assert_eq!(left, 0);
  let _ = std::fs::remove_file(&path);
}
