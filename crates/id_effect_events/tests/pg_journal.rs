//! `SqlEventJournal` over PostgreSQL (feature `pg`) at `DATABASE_URL`.
//!
//! Without a database the tests print `SKIP` and return, unless `ID_EFFECT_REQUIRE_PG=1` (CI),
//! where a missing or unreachable database fails them.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Ping, run};
use id_effect_events::{EventStore, EventStoreError, SqlEventJournal};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};

fn skip(test: &str, why: &str) {
  assert!(
    !std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1"),
    "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
}

/// A journal on `DATABASE_URL` with its table created, or `None` after [`skip`].
async fn journal(test: &str) -> Option<SqlEventJournal<Ping>> {
  let Ok(url) = std::env::var("DATABASE_URL") else {
    skip(test, "DATABASE_URL is not set");
    return None;
  };
  let config = PgPoolConfig::from_url(url).with_max_size(8);
  let pool = match tokio::time::timeout(Duration::from_secs(5), pg_pool_from_config(config)).await {
    Ok(Ok(pool)) => pool,
    Ok(Err(e)) => {
      skip(test, &format!("cannot build pool: {}", e.0));
      return None;
    }
    Err(_) => {
      skip(test, "timed out connecting");
      return None;
    }
  };
  let journal = SqlEventJournal::new(Arc::new(PgSqlClient::new(pool)));
  // Tests run as concurrent processes: two `CREATE TABLE IF NOT EXISTS` can race on the catalog,
  // so retry the loser once the winner has created the table.
  let mut attempts = 0;
  loop {
    match run(journal.create_table()).await {
      Ok(()) => return Some(journal),
      Err(e) if attempts < 5 => {
        attempts += 1;
        eprintln!("{test}: retrying journal DDL after {e}");
        tokio::time::sleep(Duration::from_millis(100)).await;
      }
      Err(e) => panic!("{test}: journal DDL: {e}"),
    }
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_round_trip() {
  let Some(journal) = journal("pg_round_trip").await else {
    return;
  };
  common::round_trip(&journal).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn expected_version_conflict() {
  let Some(journal) = journal("expected_version_conflict").await else {
    return;
  };
  common::expected_version_conflict(&journal).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_dispatch_over_journal() {
  let Some(journal) = journal("pg_dispatch_over_journal").await else {
    return;
  };
  common::dispatch_over_journal(&journal).await;
}

/// Concurrent writers at the same expected version: exactly one wins, the others get a
/// `VersionConflict` (from the version check, or from the primary key when they raced past it).
#[tokio::test(flavor = "multi_thread")]
async fn pg_concurrent_appends_at_one_version_have_one_winner() {
  let Some(journal) = journal("pg_concurrent_appends_at_one_version_have_one_winner").await else {
    return;
  };
  let s = common::stream("race");
  let writers: Vec<_> = (0..8u32)
    .map(|n| {
      let (journal, s) = (journal.clone(), s.clone());
      tokio::spawn(async move { run(journal.append_expected(&s, 0, &[Ping::Up(n)])).await })
    })
    .collect();
  let mut wins = 0;
  for writer in writers {
    match writer.await.expect("writer task") {
      Ok(stored) => {
        wins += 1;
        assert_eq!(stored[0].version, 1);
      }
      Err(e) => assert!(
        matches!(
          e,
          EventStoreError::VersionConflict {
            expected: 0,
            actual: 1,
            ..
          }
        ),
        "{e}"
      ),
    }
  }
  assert_eq!(wins, 1);
  assert_eq!(run(journal.latest_version(&s)).await.expect("latest"), 1);
}
