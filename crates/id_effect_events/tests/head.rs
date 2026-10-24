//! `EventLog::head` (r13-f-eventlog-head, 0.12 follow-up F21): the position of the last event
//! appended, `0` while the log is empty. The in-memory store and the SQL journal answer in one
//! step; the provided implementation (a log that only implements `read_all`) gives the same
//! answer by reading the log to its end.
//!
//! The PostgreSQL test runs against a container this test binary starts itself (never the
//! ambient `DATABASE_URL`). Without Docker it prints `SKIP` and passes, unless
//! `ID_EFFECT_REQUIRE_PG=1` (or `ID_EFFECT_REQUIRE_DOCKER=1`), where it fails.

use std::sync::Arc;

use id_effect::{Cause, Effect, Exit};
use id_effect_events::{
  EventLog, EventStore, EventStoreError, MemoryEventStore, RecordedEvent, SqlEventJournal,
};
use id_effect_tokio::TokioRuntime;

/// Runs `eff` on the test's tokio runtime (sqlx needs one): the typed outcome as a `Result`.
async fn run<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(
  eff: Effect<A, E>,
) -> Result<A, E> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// A log that only implements `read_all`: its `head` is the provided one.
struct ReadAllOnly<L>(L);

impl<L: EventLog<i64>> EventLog<i64> for ReadAllOnly<L> {
  fn read_all(&self, after: u64, limit: usize) -> Effect<Vec<RecordedEvent<i64>>, EventStoreError> {
    self.0.read_all(after, limit)
  }
}

fn unique(name: &str) -> String {
  format!("{name}-{}", uuid::Uuid::new_v4())
}

/// After appends to two streams, `head` is the position of the last event `read_all` returns,
/// and nothing lies after it; one more append moves it by the number of events appended.
async fn head_tracks_appends<S>(store: S)
where
  S: EventStore<i64> + EventLog<i64> + Clone + 'static,
{
  let start = run(store.head()).await.expect("head");
  let (a, b) = (unique("a"), unique("b"));
  run(store.append(&a, &[1, 2])).await.expect("a");
  run(store.append(&b, &[3])).await.expect("b");
  let head = run(store.head()).await.expect("head");
  assert_eq!(head, start + 3);
  let all = run(store.read_all(0, usize::MAX)).await.expect("read_all");
  assert_eq!(all.last().map(|e| e.position), Some(head));
  assert_eq!(all.last().map(|e| e.stream_id.as_str()), Some(b.as_str()));
  assert!(
    run(store.read_all(head, 10))
      .await
      .expect("after head")
      .is_empty()
  );

  run(store.append(&a, &[4, 5])).await.expect("a again");
  let moved = run(store.head()).await.expect("moved");
  assert_eq!(moved, head + 2);
  let tail = run(store.read_all(head, 10)).await.expect("tail");
  assert_eq!(
    tail.iter().map(|e| e.event.payload).collect::<Vec<_>>(),
    [4, 5]
  );

  // Through `Arc` and `dyn`, and by the provided implementation: the same head.
  let shared: Arc<dyn EventLog<i64>> = Arc::new(store.clone());
  assert_eq!(run(shared.head()).await.expect("dyn"), moved);
  assert_eq!(
    run(ReadAllOnly(store).head()).await.expect("provided"),
    moved
  );
}

mod memory {
  use super::*;

  #[tokio::test(flavor = "multi_thread")]
  async fn head_is_last_global_position() {
    let store = MemoryEventStore::<i64>::new();
    assert_eq!(run(store.head()).await.expect("empty"), 0);
    head_tracks_appends(store).await;
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn head_default_impl_matches_override() {
    let store = MemoryEventStore::<i64>::new();
    let provided = ReadAllOnly(store.clone());
    assert_eq!(run(provided.head()).await.expect("empty"), 0);
    for (n, stream) in ["x", "y", "x", "z", "y"].into_iter().enumerate() {
      let values: Vec<i64> = (0..=i64::try_from(n).expect("small")).collect();
      run(store.append(stream, &values)).await.expect("append");
      let over = run(store.head()).await.expect("override");
      assert_eq!(run(provided.head()).await.expect("provided"), over);
    }
    assert_eq!(run(store.head()).await.expect("head"), 15);

    // The provided head fails as its read fails.
    struct Broken;
    impl EventLog<i64> for Broken {
      fn read_all(&self, _: u64, _: usize) -> Effect<Vec<RecordedEvent<i64>>, EventStoreError> {
        id_effect::fail(EventStoreError::Io("down".into()))
      }
    }
    assert_eq!(
      run(Broken.head()).await.expect_err("broken"),
      EventStoreError::Io("down".into())
    );
  }
}

mod pg {
  use super::*;
  use id_effect::HNil;
  use id_effect_test::{ContainerError, Postgres, Shared, docker_available};

  static PG: Shared<Postgres> = Shared::new(Postgres::new);

  fn pg_required() -> bool {
    ["ID_EFFECT_REQUIRE_PG", "ID_EFFECT_REQUIRE_DOCKER"]
      .iter()
      .any(|var| std::env::var(var).is_ok_and(|v| v == "1"))
  }

  /// A journal on this binary's own PostgreSQL container, or `None` (SKIP) without Docker.
  async fn pg_journal(test: &str) -> Option<SqlEventJournal<i64>> {
    if let Err(e) = tokio::task::spawn_blocking(docker_available)
      .await
      .expect("probe")
    {
      assert!(
        !pg_required(),
        "{test}: ID_EFFECT_REQUIRE_PG=1 but Docker is unavailable: {e}"
      );
      eprintln!("SKIP {test}: {e} (set ID_EFFECT_REQUIRE_PG=1 to require PostgreSQL)");
      return None;
    }
    let pg = run(PG.get::<ContainerError, HNil>())
      .await
      .expect("start PostgreSQL");
    let env = run(pg.sql_layer::<EventStoreError>().build())
      .await
      .expect("connect");
    let journal = SqlEventJournal::from_service(&env.0);
    run(journal.create_table()).await.expect("journal table");
    Some(journal)
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn head_is_last_global_position() {
    if let Some(journal) = pg_journal("pg::head_is_last_global_position").await {
      head_tracks_appends(journal).await;
    }
  }
}
