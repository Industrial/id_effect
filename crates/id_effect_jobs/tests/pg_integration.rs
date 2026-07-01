//! PostgreSQL integration smoke tests (skipped when `DATABASE_URL` is unset).

#![cfg(all(feature = "apalis", feature = "obix"))]

use id_effect::run_async;
use id_effect_jobs::{ApalisJobQueue, JobSpec, JobsOutboxEvent, ObixOutbox, OutboxRecord};
use obix::prelude::es_entity;
use obix::{MailboxConfig, OutboxEventHandler, OutboxEventJobConfig, out::PersistentOutboxEvent};
use sqlx::PgPool;

async fn maybe_pool() -> Option<PgPool> {
  let url = std::env::var("DATABASE_URL").ok()?;
  PgPool::connect(&url).await.ok()
}

#[tokio::test]
async fn apalis_enqueue_smoke() {
  let Some(pool) = maybe_pool().await else {
    eprintln!("pg_integration: DATABASE_URL unset — skipping apalis_enqueue_smoke");
    return;
  };

  ApalisJobQueue::setup(&pool).await.expect("apalis setup");
  let queue = ApalisJobQueue::new(&pool, "id_effect_jobs_test");
  let record = run_async(queue.enqueue(JobSpec::new("smoke", b"hello")), ())
    .await
    .expect("enqueue");
  assert_eq!(record.spec.name, "smoke");
}

/// Handler used only to prove the native relay wiring compiles and registers.
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
  let Some(pool) = maybe_pool().await else {
    eprintln!("pg_integration: DATABASE_URL unset — skipping obix_outbox_smoke");
    return;
  };

  let config = MailboxConfig::builder().build().expect("mailbox config");
  let outbox = ObixOutbox::init(&pool, config).await.expect("obix init");

  let row = OutboxRecord::new("agg-1", "SmokeEvent", br#"{"ok":true}"#);
  let stored = run_async(outbox.insert(row), ()).await.expect("insert");
  assert_eq!(stored.event_type, "SmokeEvent");
  assert!(!stored.id.is_empty());

  // Wiring smoke: registering an obix-native handler through the new API path.
  // End-to-end drain coverage lives in obix's own `outbox_event_handler` tests.
  let job_config = ::job::JobSvcConfig::builder()
    .pool(pool.clone())
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
