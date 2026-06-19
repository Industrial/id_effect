//! PostgreSQL integration smoke tests (skipped when `DATABASE_URL` is unset).

#![cfg(all(feature = "apalis", feature = "obix"))]

use id_effect::run_async;
use id_effect_jobs::{
  ApalisJobQueue, JobSpec, JobsOutboxEvent, ObixOutbox, OutboxRecord, OutboxTable,
};
use obix::MailboxConfig;
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

#[tokio::test]
async fn obix_outbox_round_trip() {
  let Some(pool) = maybe_pool().await else {
    eprintln!("pg_integration: DATABASE_URL unset — skipping obix_outbox_round_trip");
    return;
  };

  let config = MailboxConfig::builder().build().expect("mailbox config");
  let outbox = ObixOutbox::init(&pool, config).await.expect("obix init");
  let row = OutboxRecord::new("agg-1", "SmokeEvent", br#"{"ok":true}"#);
  let stored = run_async(outbox.insert(row), ()).await.expect("insert");
  assert_eq!(stored.event_type, "SmokeEvent");

  let batch = run_async(outbox.fetch_unpublished(10), ())
    .await
    .expect("fetch");
  assert!(!batch.is_empty());

  let ids: Vec<String> = batch.iter().map(|r| r.id.clone()).collect();
  run_async(outbox.mark_published(&ids), ())
    .await
    .expect("mark published");

  let _payload: JobsOutboxEvent = serde_json::from_value(serde_json::json!({
    "id": stored.id,
    "aggregate_id": stored.aggregate_id,
    "event_type": stored.event_type,
    "payload": stored.payload,
    "created_ms": stored.created_ms,
  }))
  .expect("payload shape");
}
