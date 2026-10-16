//! `Postgres::sql_layer` (feature `sql`): a container and a ready `SqlClient` over it as one
//! scoped layer, per test or over a shared container.
//!
//! Without Docker the tests print `SKIP` and pass, unless `ID_EFFECT_REQUIRE_DOCKER=1`.

#![cfg(feature = "sql")]

use id_effect::{Cause, Effect, Exit, caps, need};
use id_effect_sql::{SqlClientService, SqlError, SqlValue, execute, query};
use id_effect_test::{
  ContainerError, Postgres, PostgresContainer, Shared, container_exists, require_docker,
};
use id_effect_tokio::TokioRuntime;

#[derive(Debug)]
enum TestError {
  Container(ContainerError),
  Sql(SqlError),
}

impl std::fmt::Display for TestError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Container(e) => write!(f, "{e}"),
      Self::Sql(e) => write!(f, "{e}"),
    }
  }
}

impl From<ContainerError> for TestError {
  fn from(e: ContainerError) -> Self {
    Self::Container(e)
  }
}

impl From<SqlError> for TestError {
  fn from(e: SqlError) -> Self {
    Self::Sql(e)
  }
}

async fn run<A: Send + 'static>(eff: Effect<A, TestError>) -> A {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => a,
    Exit::Failure(Cause::Fail(e)) => panic!("failed: {e}"),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

type Env = caps!(SqlClientService, PostgresContainer);

/// Writes and reads back a row; returns the container id and the value read.
fn round_trip() -> Effect<(String, i64), TestError, Env> {
  execute("CREATE TABLE IF NOT EXISTS t (n BIGINT)", vec![])
    .flat_map(|_| execute("INSERT INTO t VALUES ($1)", vec![SqlValue::I64(42)]))
    .flat_map(|_| query("SELECT n FROM t LIMIT 1", vec![]))
    .map_err(TestError::from)
    .flat_map(|rows| {
      let n = rows[0].get::<i64>("n");
      need::<PostgresContainer>().map(move |pg| (pg.container().id().to_owned(), n))
    })
    .flat_map(|(id, n)| match n {
      Ok(n) => id_effect::succeed((id, n)),
      Err(e) => id_effect::fail(TestError::from(e)),
    })
}

async fn exists(id: &str) -> bool {
  run(container_exists(id)).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sql_layer_queries_the_container_then_removes_it() {
  if !require_docker() {
    return;
  }
  let (id, n) = run(round_trip().provide_layer(Postgres::new().sql_layer())).await;
  assert_eq!(n, 42);
  assert!(
    !exists(&id).await,
    "the pool closes, then the container is removed"
  );
}

static SHARED_PG: Shared<Postgres> = Shared::new(Postgres::new);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_sql_layer_reuses_one_container() {
  if !require_docker() {
    return;
  }
  let (first, n) = run(round_trip().provide_layer(SHARED_PG.sql_layer())).await;
  let (second, _) = run(round_trip().provide_layer(SHARED_PG.sql_layer())).await;
  assert_eq!((first.as_str(), n), (second.as_str(), 42));
  assert!(
    exists(&first).await,
    "the shared container outlives both layers"
  );

  // A client over a container the test already holds.
  let pg = run(SHARED_PG.get()).await;
  let one: Effect<_, TestError, caps!(SqlClientService)> =
    query("SELECT 1::int8 AS one", vec![]).map_err(TestError::from);
  let one = one.provide_layer(pg.sql_layer());
  let rows = run(one).await;
  assert_eq!(rows[0].get::<i64>("one"), Ok(1));
}
