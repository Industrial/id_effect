//! Journal checks shared by the SQLite (`tests/sql_journal.rs`) and PostgreSQL
//! (`tests/pg_journal.rs`) suites: the same `SqlEventJournal` over each driver.

use id_effect::{Cause, Effect, Exit};
use id_effect_events::{
  CommandHandler, EventStore, EventStoreError, Projection, SqlEventJournal, dispatch_command,
  run_projection_from_store,
};
use id_effect_tokio::TokioRuntime;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum Ping {
  Up(u32),
  Down(u32),
}

pub struct Level;

impl Projection<i64, Ping> for Level {
  fn initial(&self) -> i64 {
    0
  }
  fn apply(&self, level: i64, event: &Ping) -> i64 {
    match event {
      Ping::Up(n) => level + i64::from(*n),
      Ping::Down(n) => level - i64::from(*n),
    }
  }
}

pub struct Raise;

impl CommandHandler<u32, Ping> for Raise {
  type Error = std::io::Error;
  fn handle(&self, by: u32) -> Effect<Vec<Ping>, Self::Error> {
    id_effect::succeed(vec![Ping::Up(by)])
  }
}

/// Runs `eff` on the test's tokio runtime (sqlx needs one): the typed outcome as a `Result`.
pub async fn run<A: Send + 'static, E: Send + 'static + std::fmt::Debug>(
  eff: Effect<A, E>,
) -> Result<A, E> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// A fresh stream id (the PostgreSQL suite shares one database between runs).
pub fn stream(name: &str) -> String {
  format!("{name}-{}", uuid::Uuid::new_v4())
}

/// Append, read back, latest version, and a projection folded from the stored stream.
pub async fn round_trip(journal: &SqlEventJournal<Ping>) {
  let s = stream("round-trip");
  let stored = run(journal.append(&s, &[Ping::Up(3), Ping::Down(1)]))
    .await
    .expect("append");
  assert_eq!(stored.iter().map(|e| e.version).collect::<Vec<_>>(), [1, 2]);
  let read = run(journal.read(&s, 1)).await.expect("read");
  assert_eq!(read, stored);
  assert_eq!(
    run(journal.read(&s, 2)).await.expect("tail")[0].payload,
    Ping::Down(1)
  );
  assert_eq!(run(journal.latest_version(&s)).await.expect("latest"), 2);
  assert_eq!(
    run(journal.latest_version(&stream("empty")))
      .await
      .expect("empty"),
    0
  );
  let level = run(run_projection_from_store(journal, &s, 1, Level))
    .await
    .expect("project");
  assert_eq!(level, 2);
}

/// `append_expected` at a stale version appends nothing and names the version found.
pub async fn expected_version_conflict(journal: &SqlEventJournal<Ping>) {
  let s = stream("conflict");
  run(journal.append_expected(&s, 0, &[Ping::Up(1)]))
    .await
    .expect("append at 0");
  let err = run(journal.append_expected(&s, 0, &[Ping::Up(2), Ping::Up(3)]))
    .await
    .expect_err("stale");
  assert_eq!(
    err,
    EventStoreError::VersionConflict {
      stream_id: s.clone(),
      expected: 0,
      actual: 1,
    }
  );
  assert_eq!(run(journal.latest_version(&s)).await.expect("latest"), 1);
  let err = run(journal.append_expected(&s, 5, &[Ping::Up(2)]))
    .await
    .expect_err("ahead");
  assert!(matches!(
    err,
    EventStoreError::VersionConflict {
      expected: 5,
      actual: 1,
      ..
    }
  ));
  let next = run(journal.append_expected(&s, 1, &[Ping::Up(2)]))
    .await
    .expect("append at 1");
  assert_eq!(next[0].version, 2);
}

/// `dispatch_command` over the journal: the handler's events are stored and folded.
pub async fn dispatch_over_journal(journal: &SqlEventJournal<Ping>) {
  let s = stream("dispatch");
  let level = run(dispatch_command(&Raise, journal, &s, Level, 4))
    .await
    .expect("dispatch");
  assert_eq!(level, 4);
  let level = run(dispatch_command(&Raise, journal, &s, Level, 6))
    .await
    .expect("dispatch again");
  assert_eq!(level, 10);
  assert_eq!(run(journal.latest_version(&s)).await.expect("latest"), 2);
}
