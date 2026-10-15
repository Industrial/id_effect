//! End-to-end async messaging demo (memory + optional Postgres).
//!
//! ```bash
//! cargo run -p id_effect_jobs --example 010_messaging_e2e --features memory,apalis,obix
//! ```

use id_effect::{Effect, succeed};
use id_effect_jobs::{
  JobRunner, JobSpec, MemoryJobRunner, MemoryOutbox, OutboxRecord, OutboxTable, drain_jobs,
  relay_outbox,
};
use id_effect_tokio::TokioRuntime;

#[cfg(feature = "apalis")]
use id_effect_jobs::ApalisJobQueue;

#[cfg(feature = "obix")]
use id_effect_jobs::ObixOutbox;

/// Runs `eff` on the ambient tokio runtime; a typed failure becomes `Err`.
async fn run<A, E>(eff: Effect<A, E>) -> Result<A, Box<dyn std::error::Error>>
where
  A: Send + 'static,
  E: std::error::Error + Send + 'static,
{
  let exit = TokioRuntime::current()?.run(eff).await;
  exit
    .into_result()
    .map_err(|cause| match cause.fail_option() {
      Some(e) => Box::new(e) as Box<dyn std::error::Error>,
      None => "effect died or was interrupted".into(),
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let runner = MemoryJobRunner::new();
  run(runner.enqueue(JobSpec::new("demo", b"hello".as_slice()))).await?;
  let n = run(drain_jobs(runner.clone(), 10, |spec| {
    println!("handled job {}", spec.name);
    succeed::<(), id_effect_jobs::JobError, _>(())
  }))
  .await?;
  println!("memory jobs processed: {n}");

  let outbox = MemoryOutbox::new();
  run(outbox.insert(OutboxRecord::new("agg-1", "DemoEvent", br#"{"ok":true}"#))).await?;
  let relayed = run(relay_outbox(outbox.clone(), 10, |row| {
    println!("relay {} {}", row.event_type, row.aggregate_id);
    succeed::<(), id_effect_jobs::OutboxError, _>(())
  }))
  .await?;
  println!("memory outbox relayed: {relayed}");

  #[cfg(all(feature = "apalis", feature = "obix"))]
  if let Ok(url) = std::env::var("DATABASE_URL") {
    use id_effect_sql::pg::{PgPoolConfig, pg_pool_from_config};
    use obix::MailboxConfig;
    let pool = pg_pool_from_config(PgPoolConfig::from_url(url.clone()).with_max_size(4))
      .await
      .map_err(|e| e.0.to_string())?;
    // apalis-postgres 1.0 is on sqlx 0.9: it gets its own pool on the same database.
    let apalis_pool = apalis_postgres::PgPool::connect(&url).await?;
    ApalisJobQueue::setup(&apalis_pool).await?;
    let queue = ApalisJobQueue::new(&apalis_pool, "demo");
    run(queue.enqueue(JobSpec::new("pg_job", b"pg".as_slice()))).await?;
    let config = MailboxConfig::builder()
      .build()
      .map_err(|e| e.to_string())?;
    let obix = ObixOutbox::init(&pool, config).await?;
    run(obix.insert(OutboxRecord::new("agg-pg", "PgEvent", b"{}".as_slice()))).await?;
    println!("postgres apalis + obix round-trip ok");
  }

  Ok(())
}
