//! `SqlEventJournal` over SQLite (feature `sqlite`): in-memory and file databases, no server.

mod common;
mod events;

use std::sync::Arc;

use common::{Ping, run};
use id_effect_events::{CatchUpSubscription, EventStore, SqlEventJournal};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};

async fn journal(config: SqlitePoolConfig) -> SqlEventJournal<Ping> {
  let pool = sqlite_pool_from_config(config).await.expect("sqlite pool");
  let journal = SqlEventJournal::new(Arc::new(SqliteSqlClient::new(pool)));
  events::create_tables(&journal).await.expect("ddl");
  // Idempotent: a second run is a no-op.
  events::create_tables(&journal).await.expect("ddl again");
  journal
}

async fn memory() -> SqlEventJournal<Ping> {
  journal(SqlitePoolConfig::memory()).await
}

#[tokio::test(flavor = "multi_thread")]
async fn sqlite_round_trip() {
  common::round_trip(&memory().await).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn expected_version_conflict() {
  common::expected_version_conflict(&memory().await).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sqlite_dispatch_over_journal() {
  common::dispatch_over_journal(&memory().await).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn aggregate_loads_from_snapshot_plus_tail() {
  events::aggregate_loads_from_snapshot_plus_tail(&memory().await).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subscription_resumes_from_checkpoint() {
  events::subscription_resumes_from_checkpoint(&memory().await).await;
}

/// The checkpoint is in the database: a subscription in a reopened database continues after it.
#[tokio::test(flavor = "multi_thread")]
async fn sqlite_subscription_resumes_after_reopen() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("events.sqlite");
  let s = common::stream("resume");
  let seen: events::Seen = Arc::default();
  {
    let journal = journal(SqlitePoolConfig::file(&path)).await;
    run(journal.append(&s, &[Ping::Up(1), Ping::Up(2)]))
      .await
      .expect("append");
    let (_, checkpoints) = events::stores(&journal);
    let sub = CatchUpSubscription::new("projector", journal.clone(), checkpoints);
    let handled = run(sub.catch_up(events::recorder(&seen, &[&s])))
      .await
      .expect("catch up");
    assert_eq!(handled, 2);
    run(journal.append(&s, &[Ping::Down(1)]))
      .await
      .expect("append before restart");
  }
  let reopened = journal(SqlitePoolConfig::file(&path)).await;
  let (_, checkpoints) = events::stores(&reopened);
  let sub = CatchUpSubscription::new("projector", reopened.clone(), checkpoints);
  assert_eq!(run(sub.checkpoint()).await.expect("stored"), 2);
  let handled = run(sub.catch_up(events::recorder(&seen, &[&s])))
    .await
    .expect("resume");
  assert_eq!(handled, 1);
  let positions: Vec<u64> = seen.lock().expect("seen").iter().map(|e| e.0).collect();
  assert_eq!(positions, [1, 2, 3]);
}

#[tokio::test(flavor = "multi_thread")]
async fn sqlite_file_journal_survives_reopen() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("events.sqlite");
  let s = common::stream("durable");
  {
    let journal = journal(SqlitePoolConfig::file(&path)).await;
    run(journal.append_expected(&s, 0, &[Ping::Up(1), Ping::Up(2)]))
      .await
      .expect("append");
  }
  let reopened = journal(SqlitePoolConfig::file(&path)).await;
  let read = run(reopened.read(&s, 1)).await.expect("read");
  assert_eq!(read.len(), 2);
  assert_eq!(read[1].payload, Ping::Up(2));
  assert_eq!(run(reopened.latest_version(&s)).await.expect("latest"), 2);
}
