//! `SqlEventJournal` over SQLite (feature `sqlite`): in-memory and file databases, no server.

mod common;

use std::sync::Arc;

use common::{Ping, run};
use id_effect_events::{EventStore, SqlEventJournal};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};

async fn journal(config: SqlitePoolConfig) -> SqlEventJournal<Ping> {
  let pool = sqlite_pool_from_config(config).await.expect("sqlite pool");
  let journal = SqlEventJournal::new(Arc::new(SqliteSqlClient::new(pool)));
  run(journal.create_table()).await.expect("ddl");
  // Idempotent: a second run is a no-op.
  run(journal.create_table()).await.expect("ddl again");
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
