//! PostgreSQL integration smoke tests.
//!
//! They need a database at `DATABASE_URL`. With `ID_EFFECT_REQUIRE_PG=1` a missing or
//! unreachable database is a test **failure**; without it the tests print `SKIP` and return.
//! Effects run on `id_effect_tokio::TokioRuntime` (sqlx needs tokio's IO driver).

#![cfg(feature = "apalis")]

use id_effect::Effect;
use id_effect_jobs::{ApalisJobQueue, JobSpec};
use id_effect_tokio::TokioRuntime;
use sqlx::PgPool;

fn pg_required() -> bool {
  std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
}

async fn maybe_pool(test: &str) -> Option<PgPool> {
  let why = match std::env::var("DATABASE_URL") {
    Ok(url) => match PgPool::connect(&url).await {
      Ok(pool) => return Some(pool),
      Err(e) => e.to_string(),
    },
    Err(_) => "DATABASE_URL unset".to_string(),
  };
  assert!(
    !pg_required(),
    "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
  );
  eprintln!("SKIP pg_integration {test}: {why}");
  None
}

async fn run<A, E>(eff: Effect<A, E>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + std::fmt::Debug + 'static,
{
  TokioRuntime::current()
    .expect("tokio")
    .run(eff)
    .await
    .into_result()
    .map_err(|c| c.fail_option().expect("typed failure"))
}

#[tokio::test]
async fn apalis_enqueue_smoke() {
  if maybe_pool("apalis_enqueue_smoke").await.is_none() {
    return;
  }
  // apalis-postgres 1.0 is on sqlx 0.9: it gets its own (0.9) pool on the same database.
  let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
  let pool = apalis_postgres::PgPool::connect(&url)
    .await
    .expect("apalis pool");

  ApalisJobQueue::setup(&pool).await.expect("apalis setup");
  let queue = ApalisJobQueue::new(&pool, "id_effect_jobs_test");
  let record = run(queue.enqueue(JobSpec::new("smoke", b"hello")))
    .await
    .expect("enqueue");
  assert_eq!(record.spec.name, "smoke");
}
