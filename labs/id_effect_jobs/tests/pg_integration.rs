//! PostgreSQL integration smoke tests.
//!
//! They need a database at `DATABASE_URL`. With `ID_EFFECT_REQUIRE_PG=1` a missing or
//! unreachable database is a test **failure**; without it the tests print `SKIP` and return.
//! Effects run on `id_effect_tokio::TokioRuntime` (sqlx needs tokio's IO driver).

#![cfg(all(feature = "apalis", feature = "obix"))]

use id_effect::next::Effect;
use id_effect_jobs::{ApalisJobQueue, JobSpec, JobsOutboxEvent, ObixOutbox, OutboxRecord};
use id_effect_tokio::TokioRuntime;
use obix::prelude::es_entity;
use obix::{MailboxConfig, OutboxEventHandler, OutboxEventJobConfig, out::PersistentOutboxEvent};
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

/// obix runs no migrations of its own: apply its schema (a fixture copied from the locked obix
/// release) once, when its outbox table is missing.
async fn ensure_obix_schema(pool: &PgPool) {
  let (present,): (bool,) =
    sqlx::query_as("SELECT to_regclass('public.persistent_outbox_events') IS NOT NULL")
      .fetch_one(pool)
      .await
      .expect("probe obix schema");
  if !present {
    sqlx::raw_sql(include_str!("fixtures/obix_0.2.33_setup.sql"))
      .execute(pool)
      .await
      .expect("apply obix schema");
  }
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

struct NoopHandler;

impl OutboxEventHandler<JobsOutboxEvent> for NoopHandler {
  async fn handle_persistent(
    &self,
    _op: &mut es_entity::DbOp<'_>,
    _event: &PersistentOutboxEvent<JobsOutboxEvent>,
  ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Ok(())
  }
}

#[tokio::test]
async fn obix_outbox_smoke() {
  let Some(pool) = maybe_pool("obix_outbox_smoke").await else {
    return;
  };
  ensure_obix_schema(&pool).await;

  let config = MailboxConfig::builder().build().expect("mailbox config");
  let outbox = ObixOutbox::init(&pool, config).await.expect("obix init");

  let row = OutboxRecord::new("agg-1", "SmokeEvent", br#"{"ok":true}"#);
  let stored = run(outbox.insert(row)).await.expect("insert");
  assert_eq!(stored.event_type, "SmokeEvent");
  assert!(!stored.id.is_empty());

  let job_config = ::job::JobSvcConfig::builder()
    .pool(pool.clone())
    .exec_migrations(true)
    .build()
    .expect("job svc config");
  let mut jobs = ::job::Jobs::init(job_config).await.expect("jobs init");
  outbox
    .register_event_handler(
      &mut jobs,
      OutboxEventJobConfig::new(::job::JobType::new("id_effect_jobs_obix_smoke")),
      NoopHandler,
    )
    .await
    .expect("register handler");

  let _payload: JobsOutboxEvent = serde_json::from_value(serde_json::json!({
    "id": stored.id,
    "aggregate_id": stored.aggregate_id,
    "event_type": stored.event_type,
    "payload": stored.payload,
    "created_ms": stored.created_ms,
  }))
  .expect("payload shape");
}
