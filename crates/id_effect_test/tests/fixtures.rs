//! Fixtures and factories (feature `fixtures`): deterministic Schema-arbitrary defaults,
//! overrides, and inserts read back with `FromRow`, on SQLite inside `isolated_db()` (no Docker).

#![cfg(feature = "fixtures")]

use std::path::PathBuf;

use id_effect::schema::{i64, refine, string};
use id_effect::{Cause, Effect, Exit, caps};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient};
use id_effect_sql::{FromRow, SqlClientService, SqlError, SqlValue, execute, params, query_one_as};
use id_effect_test::fixtures::{BoxedStrategy, DEFAULT_SEED, Strategy, from_schema, proptest};
use id_effect_test::{Factory, Fixture, isolated_db};
use id_effect_tokio::TokioRuntime;

#[derive(Clone, Debug, PartialEq, FromRow)]
struct Account {
  id: i64,
  email: String,
  balance: i64,
}

#[derive(Clone, Debug, PartialEq)]
struct NewAccount {
  email: String,
  balance: i64,
}

fn email() -> impl Strategy<Value = String> {
  from_schema(
    refine(string::<()>(), |s: &String| s.contains('@'), "an email"),
    "[a-z]{1,8}@[a-z]{1,8}\\.test",
  )
}

impl Fixture for NewAccount {
  type Row = Account;

  fn arbitrary() -> BoxedStrategy<Self> {
    let balance = from_schema(
      refine(i64::<()>(), |n| *n >= 0, "non-negative"),
      -100..10_000i64,
    );
    (email(), balance)
      .prop_map(|(email, balance)| NewAccount { email, balance })
      .boxed()
  }

  fn insert_sql(&self) -> (String, Vec<SqlValue>) {
    (
      "INSERT INTO accounts (email, balance) VALUES ($1, $2) RETURNING id, email, balance".into(),
      params![self.email.clone(), self.balance],
    )
  }
}

/// A fixture inserted by an overridden `insert` (two statements, a count back).
#[derive(Clone, Debug)]
struct Pair;

impl Fixture for Pair {
  type Row = (i64,);

  fn arbitrary() -> BoxedStrategy<Self> {
    proptest::strategy::Just(Pair).boxed()
  }

  fn insert_sql(&self) -> (String, Vec<SqlValue>) {
    unreachable!("Pair inserts through `insert`")
  }

  fn insert<R, I>(&self) -> Effect<(i64,), SqlError, R>
  where
    R: id_effect::Selector<SqlClientService, I> + id_effect::HList,
  {
    let one = "INSERT INTO accounts (email, balance) VALUES ('a@pair.test', 1)";
    let two = "INSERT INTO accounts (email, balance) VALUES ('b@pair.test', 2)";
    execute(one, vec![])
      .flat_map(move |_| execute(two, vec![]))
      .flat_map(|_| query_one_as("SELECT COUNT(*) FROM accounts", vec![]))
  }
}

type Db = caps!(SqlClientService);

/// A fresh SQLite file with the `accounts` table (removed on drop).
struct Accounts(PathBuf);

impl Accounts {
  async fn new(name: &str) -> Self {
    let path = std::env::temp_dir().join(format!(
      "id_effect_test_fixtures_{name}_{}.sqlite",
      std::process::id()
    ));
    let accounts = Self(path);
    accounts.remove();
    let ddl: Effect<u64, SqlError, Db> = execute(
      "CREATE TABLE accounts (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, balance INTEGER NOT NULL)",
      vec![],
    );
    run(ddl.provide_layer(SqliteSqlClient::layer(accounts.config())))
      .await
      .expect("schema");
    accounts
  }

  fn config(&self) -> SqlitePoolConfig {
    let mut config = SqlitePoolConfig::file(&self.0);
    // Opening a WAL database file syncs it several times: generous on a loaded CI machine.
    config.acquire_timeout = std::time::Duration::from_secs(120);
    config.busy_timeout = std::time::Duration::from_secs(60);
    config
  }

  fn remove(&self) {
    for suffix in ["", "-wal", "-shm"] {
      let mut path = self.0.clone().into_os_string();
      path.push(suffix);
      let _ = std::fs::remove_file(path);
    }
  }
}

impl Drop for Accounts {
  fn drop(&mut self) {
    self.remove();
  }
}

async fn run<A: Send + 'static>(eff: Effect<A, SqlError>) -> Result<A, SqlError> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

#[test]
fn the_same_seed_builds_the_same_valid_values() {
  let first = Factory::<NewAccount>::seeded(42).build_many(50);
  let again = Factory::<NewAccount>::seeded(42).build_many(50);
  assert_eq!(first, again, "deterministic");
  assert_ne!(first, Factory::<NewAccount>::seeded(43).build_many(50));
  assert!(
    first
      .iter()
      .all(|a| a.email.contains('@') && a.balance >= 0),
    "every default passes the schemas: {first:?}"
  );
  let default = Factory::<NewAccount>::new();
  assert_eq!((default.seed(), default.seq()), (DEFAULT_SEED, 0));
  assert!(format!("{default:?}").contains("NewAccount"));
}

#[test]
fn overrides_set_what_the_test_is_about() {
  let mut rich = Factory::<NewAccount>::seeded(1)
    .with(|a| a.balance = 1_000_000)
    .with_seq(|a, n| a.email = format!("user-{n}@example.test"));
  let a = rich.build();
  let b = rich.build_with(|b| b.balance = 0);
  assert_eq!(
    (a.email.as_str(), a.balance),
    ("user-1@example.test", 1_000_000)
  );
  assert_eq!((b.email.as_str(), b.balance), ("user-2@example.test", 0));
  assert_eq!(rich.seq(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inserts_return_the_from_row_decoded_rows() {
  let accounts = Accounts::new("insert").await;
  let expected = Factory::<NewAccount>::seeded(9).build_many(4);

  let mut factory = Factory::<NewAccount>::seeded(9);
  let one = factory.insert::<Db, _>();
  let two = factory.insert_with::<Db, _>(|a| a.balance = 7);
  let many = factory.insert_many::<Db, _>(2);
  let program = one.zip(two).zip(many).flat_map(|rows| {
    query_one_as::<(i64,), Db, _>("SELECT COUNT(*) FROM accounts", vec![]).map(move |n| (rows, n))
  });
  let isolated = SqliteSqlClient::layer(accounts.config()).provide_to(isolated_db());
  let (((one, two), many), (seen,)) = run(program.provide_layer(isolated)).await.expect("insert");

  assert_eq!(seen, 4);
  let row = |id: i64, a: &NewAccount| Account {
    id,
    email: a.email.clone(),
    balance: a.balance,
  };
  assert_eq!(one, row(1, &expected[0]));
  assert_eq!(
    two,
    Account {
      balance: 7,
      ..row(2, &expected[1])
    }
  );
  assert_eq!(many, vec![row(3, &expected[2]), row(4, &expected[3])]);

  // Rolled back with the test transaction.
  let left: Effect<(i64,), SqlError, Db> = query_one_as("SELECT COUNT(*) FROM accounts", vec![]);
  let left = run(left.provide_layer(SqliteSqlClient::layer(accounts.config()))).await;
  assert_eq!(left, Ok((0,)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_insert_fails_the_effect_and_a_custom_insert_is_used() {
  let accounts = Accounts::new("custom").await;
  let mut same = Factory::<NewAccount>::seeded(3).with(|a| a.email = "dup@example.test".into());
  let duplicate = same.insert_many::<Db, _>(2);
  let isolated = SqliteSqlClient::layer(accounts.config()).provide_to(isolated_db());
  let failed = run(duplicate.provide_layer(isolated)).await;
  assert!(
    matches!(failed, Err(SqlError::QueryFailed { .. })),
    "{failed:?}"
  );

  let pairs = Factory::<Pair>::new().insert::<Db, _>();
  let isolated = SqliteSqlClient::layer(accounts.config()).provide_to(isolated_db());
  assert_eq!(run(pairs.provide_layer(isolated)).await, Ok((2,)));
}
