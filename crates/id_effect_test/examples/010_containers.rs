//! Ex 010 — containers as effect resources: a PostgreSQL container with a ready `SqlClient`,
//! and a Redis container next to it, both removed when the program's scope closes.
//!
//! Needs Docker; without it the example prints `SKIP` and exits successfully (unless
//! `ID_EFFECT_REQUIRE_DOCKER=1`).
//!
//! Run: `cargo run -p id_effect_test --example 010_containers --features sql`

use id_effect::{Cause, Effect, Exit, caps, need};
use id_effect_sql::{SqlClientService, SqlError, query};
use id_effect_test::{ContainerError, ContainerImage, Postgres, Redis, RedisContainer};
use id_effect_tokio::TokioRuntime;

/// The program's error: either resource may fail.
#[derive(Debug)]
enum AppError {
  Container(ContainerError),
  Sql(SqlError),
}

impl std::fmt::Display for AppError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Container(e) => write!(f, "container: {e}"),
      Self::Sql(e) => write!(f, "sql: {e}"),
    }
  }
}

impl From<ContainerError> for AppError {
  fn from(e: ContainerError) -> Self {
    Self::Container(e)
  }
}

impl From<SqlError> for AppError {
  fn from(e: SqlError) -> Self {
    Self::Sql(e)
  }
}

/// What the program needs: a SQL client and the Redis container.
type Env = caps!(SqlClientService, RedisContainer);

/// Asks PostgreSQL for its version and reads the Redis URL.
fn program() -> Effect<(String, String), AppError, Env> {
  query("SELECT version() AS v", vec![])
    .map_err(AppError::from)
    .flat_map(|rows| {
      let version = rows[0].get::<String>("v").map_err(AppError::from);
      need::<RedisContainer>().map(move |redis| version.map(|v| (v, redis.url())))
    })
    .flat_map(|result| match result {
      Ok(pair) => id_effect::succeed(pair),
      Err(e) => id_effect::fail(e),
    })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
  if !id_effect_test::require_docker() {
    return;
  }
  let layers = Postgres::new()
    .sql_layer::<AppError>()
    .merge(Redis::new().layer());
  let runtime = TokioRuntime::current().expect("a tokio runtime");
  match runtime.run(program().provide_layer(layers)).await {
    Exit::Success((version, redis)) => {
      println!("postgres: {version}");
      println!("redis:    {redis}");
      println!("both containers are removed again");
    }
    Exit::Failure(Cause::Fail(error)) => panic!("containers example failed: {error}"),
    Exit::Failure(cause) => panic!("containers example failed: {cause:?}"),
  }
}
