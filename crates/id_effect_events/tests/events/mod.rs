//! Snapshot and catch-up subscription checks shared by the SQLite (`tests/sql_journal.rs`) and
//! PostgreSQL (`tests/pg_events.rs`) suites. The PostgreSQL database is shared with concurrent
//! test processes, so the checks only look at their own streams.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use crate::common::{Level, Ping, run, stream};
use id_effect::{Effect, succeed};
use id_effect_events::{
  CatchUpSubscription, EventStore, EventStoreError, RecordedEvent, Snapshot, SnapshotStore,
  SqlCheckpointStore, SqlEventJournal, SqlSnapshotStore, load_aggregate,
};

/// The journal's companion stores, on the same client.
pub fn stores(journal: &SqlEventJournal<Ping>) -> (SqlSnapshotStore<i64>, SqlCheckpointStore) {
  (
    SqlSnapshotStore::new(Arc::clone(journal.client())),
    SqlCheckpointStore::new(Arc::clone(journal.client())),
  )
}

/// Create the journal, snapshot and checkpoint tables (idempotent).
pub async fn create_tables(journal: &SqlEventJournal<Ping>) -> Result<(), EventStoreError> {
  let (snapshots, checkpoints) = stores(journal);
  run(journal.create_table()).await?;
  run(snapshots.create_table()).await?;
  run(checkpoints.create_table()).await
}

/// `load_aggregate` = the stored snapshot, then only the later events.
pub async fn aggregate_loads_from_snapshot_plus_tail(journal: &SqlEventJournal<Ping>) {
  let (snapshots, _) = stores(journal);
  let s = stream("snapshot");
  run(journal.append_expected(&s, 0, &[Ping::Up(10), Ping::Down(3)]))
    .await
    .expect("append");
  let loaded = run(load_aggregate(journal, &snapshots, &s, Level))
    .await
    .expect("load without snapshot");
  assert_eq!((loaded.version, loaded.state), (2, 7));
  run(snapshots.save(&loaded)).await.expect("save");
  assert_eq!(
    run(snapshots.load(&s)).await.expect("load snapshot"),
    Some(loaded.clone())
  );

  // An older snapshot does not replace it; a newer one does.
  let at = |version, state| Snapshot {
    stream_id: s.clone(),
    version,
    state,
  };
  run(snapshots.save(&at(1, -1))).await.expect("older");
  assert_eq!(run(snapshots.load(&s)).await.expect("kept"), Some(at(2, 7)));
  run(journal.append_expected(&s, 2, &[Ping::Up(5)]))
    .await
    .expect("append after snapshot");
  // A snapshot at version 3 whose state differs from the events: loading trusts it and folds
  // only what came after, which shows the events before it are not read.
  run(snapshots.save(&at(3, 100))).await.expect("newer");
  run(journal.append_expected(&s, 3, &[Ping::Up(1)]))
    .await
    .expect("tail");
  let loaded = run(load_aggregate(journal, &snapshots, &s, Level))
    .await
    .expect("load from snapshot");
  assert_eq!(loaded, at(4, 101));
  assert_eq!(
    run(snapshots.load(&stream("no-snapshot")))
      .await
      .expect("none"),
    None
  );
}

/// `(position, stream_id, event)` of each event a subscription handled.
pub type Seen = Arc<Mutex<Vec<(u64, String, Ping)>>>;

/// A handler that records the events of `streams` into `seen`.
pub fn recorder(
  seen: &Seen,
  streams: &[&str],
) -> impl Fn(&RecordedEvent<Ping>) -> Effect<(), Infallible> + Send + Sync + 'static {
  let seen = Arc::clone(seen);
  let streams: Vec<String> = streams.iter().map(|s| (*s).to_owned()).collect();
  move |e| {
    if streams.contains(&e.stream_id) {
      seen
        .lock()
        .expect("seen")
        .push((e.position, e.stream_id.clone(), e.event.payload.clone()));
    }
    succeed(())
  }
}

/// A subscription stores its checkpoint; a new subscription with the same name over the same
/// tables continues after it (no event twice, none missed), in global order across streams.
pub async fn subscription_resumes_from_checkpoint(journal: &SqlEventJournal<Ping>) {
  let (_, checkpoints) = stores(journal);
  let (a, b) = (stream("sub-a"), stream("sub-b"));
  run(journal.append(&a, &[Ping::Up(1), Ping::Up(2)]))
    .await
    .expect("a");
  run(journal.append(&b, &[Ping::Down(1)])).await.expect("b");
  let name = stream("subscriber");
  let seen: Seen = Arc::default();

  let first =
    CatchUpSubscription::new(name.clone(), journal.clone(), checkpoints.clone()).with_batch_size(2);
  run(first.catch_up(recorder(&seen, &[&a, &b])))
    .await
    .expect("catch up");
  let checkpoint = run(first.checkpoint()).await.expect("checkpoint");
  {
    let seen = seen.lock().expect("seen");
    let streams: Vec<_> = seen
      .iter()
      .map(|(_, s, e)| (s.as_str(), e.clone()))
      .collect();
    assert_eq!(
      streams,
      [
        (a.as_str(), Ping::Up(1)),
        (a.as_str(), Ping::Up(2)),
        (b.as_str(), Ping::Down(1))
      ]
    );
    assert!(seen.windows(2).all(|w| w[0].0 < w[1].0), "{seen:?}");
    assert!(checkpoint >= seen[2].0);
  }
  drop(first);

  // Restart: a fresh subscription, same name, same tables.
  run(journal.append(&a, &[Ping::Down(2)]))
    .await
    .expect("a again");
  let restarted = CatchUpSubscription::new(name, journal.clone(), checkpoints);
  run(restarted.catch_up(recorder(&seen, &[&a, &b])))
    .await
    .expect("resume");
  let resumed = run(restarted.checkpoint()).await.expect("checkpoint");
  let seen = seen.lock().expect("seen");
  assert_eq!(seen.len(), 4, "{seen:?}");
  assert_eq!(
    (seen[3].1.as_str(), &seen[3].2),
    (a.as_str(), &Ping::Down(2))
  );
  assert!(seen[3].0 > checkpoint);
  assert!(resumed >= seen[3].0);
}
