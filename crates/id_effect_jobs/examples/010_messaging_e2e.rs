//! End-to-end async messaging demo (memory + optional Postgres).
//!
//! ```bash
//! cargo run -p id_effect_jobs --example 010_messaging_e2e --features memory,apalis,obix
//! ```

use id_effect::run_async;
use id_effect_jobs::{
  JobRunner, JobSpec, MemoryJobRunner, MemoryOutbox, OutboxRecord, OutboxTable, drain_jobs,
  relay_outbox,
};

#[cfg(feature = "apalis")]
use id_effect_jobs::ApalisJobQueue;

#[cfg(feature = "obix")]
use id_effect_jobs::ObixOutbox;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let runner = MemoryJobRunner::new();
  run_async(
    runner.enqueue(JobSpec::new("demo", b"hello".as_slice())),
    (),
  )
  .await?;
  let n = run_async(
    drain_jobs(runner.clone(), 10, |spec| {
      println!("handled job {}", spec.name);
      id_effect::succeed::<(), id_effect_jobs::JobError, ()>(())
    }),
    (),
  )
  .await?;
  println!("memory jobs processed: {n}");

  let outbox = MemoryOutbox::new();
  run_async(
    outbox.insert(OutboxRecord::new("agg-1", "DemoEvent", br#"{"ok":true}"#)),
    (),
  )
  .await?;
  let relayed = run_async(
    relay_outbox(outbox.clone(), 10, |row| {
      println!("relay {} {}", row.event_type, row.aggregate_id);
      id_effect::succeed::<(), id_effect_jobs::OutboxError, ()>(())
    }),
    (),
  )
  .await?;
  println!("memory outbox relayed: {relayed}");

  #[cfg(all(feature = "apalis", feature = "obix"))]
  if let Ok(url) = std::env::var("DATABASE_URL") {
    use id_effect_sql_pg::{PgPoolConfig, pg_pool_from_config};
    use obix::MailboxConfig;
    let pool = pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(4))
      .await
      .map_err(|e| e.0.to_string())?;
    ApalisJobQueue::setup(&pool).await?;
    let queue = ApalisJobQueue::new(&pool, "demo");
    run_async(queue.enqueue(JobSpec::new("pg_job", b"pg".as_slice())), ()).await?;
    let config = MailboxConfig::builder()
      .build()
      .map_err(|e| e.to_string())?;
    let obix = ObixOutbox::init(&pool, config).await?;
    run_async(
      obix.insert(OutboxRecord::new("agg-pg", "PgEvent", b"{}".as_slice())),
      (),
    )
    .await?;
    println!("postgres apalis + obix round-trip ok");
  }

  Ok(())
}
