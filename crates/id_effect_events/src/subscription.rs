//! Checkpointed catch-up subscriptions: read every stream in global order from a stored
//! position, hand each event to a handler, and store the position reached.
//!
//! | Type | Role |
//! |------|------|
//! | [`EventLog`] | every stream's events in global position order ([`MemoryEventStore`], [`SqlEventJournal`]) |
//! | [`CheckpointStore`] | the last position a named subscription handled ([`MemoryCheckpointStore`], [`SqlCheckpointStore`]) |
//! | [`CatchUpSubscription`] | resume from the checkpoint, handle what is new, store the new checkpoint |
//!
//! Delivery is at least once: the checkpoint is stored after a batch has been handled (and,
//! when a handler fails, at the last event handled before it), so a process that stops between
//! handling and storing sees those events again on restart. Handlers should be idempotent, e.g.
//! by remembering the [`RecordedEvent::position`] they applied.
//!
//! [`MemoryEventStore`]: crate::MemoryEventStore
//! [`SqlEventJournal`]: crate::SqlEventJournal

use crate::edge::attempt;
use crate::error::EventStoreError;
use crate::event_store::StoredEvent;
use crate::sql_journal::{ready, to_sql_version, to_version};
use id_effect::{Effect, fail, succeed};
use id_effect_sql::{SqlClient, SqlClientService, params};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// An event with its place in the global order of every stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedEvent<E> {
  /// Global position (1-based, gap-free, increasing in append order).
  pub position: u64,
  /// The stream the event belongs to.
  pub stream_id: String,
  /// The event, with its version in that stream.
  pub event: StoredEvent<E>,
}

/// The events of every stream in global position order (what catch-up subscriptions read).
pub trait EventLog<E>: Send + Sync {
  /// Up to `limit` events with a position greater than `after` (`0`: from the start), in
  /// position order.
  fn read_all(&self, after: u64, limit: usize) -> Effect<Vec<RecordedEvent<E>>, EventStoreError>;
}

/// A shared log is a log.
impl<E, L: EventLog<E> + ?Sized> EventLog<E> for Arc<L> {
  fn read_all(&self, after: u64, limit: usize) -> Effect<Vec<RecordedEvent<E>>, EventStoreError> {
    (**self).read_all(after, limit)
  }
}

/// Where each named subscription got to in the [`EventLog`].
pub trait CheckpointStore: Send + Sync {
  /// The last position `subscription` handled (`0` when it never stored one).
  fn load(&self, subscription: &str) -> Effect<u64, EventStoreError>;
  /// Store `position` as the last one `subscription` handled (overwrites; storing `0` makes
  /// it start over).
  fn save(&self, subscription: &str, position: u64) -> Effect<(), EventStoreError>;
}

/// A shared checkpoint store is a checkpoint store.
impl<C: CheckpointStore + ?Sized> CheckpointStore for Arc<C> {
  fn load(&self, subscription: &str) -> Effect<u64, EventStoreError> {
    (**self).load(subscription)
  }

  fn save(&self, subscription: &str, position: u64) -> Effect<(), EventStoreError> {
    (**self).save(subscription, position)
  }
}

/// In-memory checkpoints (tests). Clones share the same checkpoints.
#[derive(Clone, Default)]
pub struct MemoryCheckpointStore {
  positions: Arc<Mutex<HashMap<String, u64>>>,
}

impl MemoryCheckpointStore {
  /// No checkpoints.
  pub fn new() -> Self {
    Self::default()
  }
}

impl CheckpointStore for MemoryCheckpointStore {
  fn load(&self, subscription: &str) -> Effect<u64, EventStoreError> {
    let (positions, subscription) = (Arc::clone(&self.positions), subscription.to_owned());
    attempt(move || {
      let guard = positions
        .lock()
        .map_err(|e| EventStoreError::Io(e.to_string()))?;
      Ok(guard.get(&subscription).copied().unwrap_or(0))
    })
  }

  fn save(&self, subscription: &str, position: u64) -> Effect<(), EventStoreError> {
    let (positions, subscription) = (Arc::clone(&self.positions), subscription.to_owned());
    attempt(move || {
      positions
        .lock()
        .map_err(|e| EventStoreError::Io(e.to_string()))?
        .insert(subscription, position);
      Ok(())
    })
  }
}

/// DDL of the checkpoint table (portable PostgreSQL / SQLite). Apply it in a migration, or at
/// startup with [`SqlCheckpointStore::create_table`].
pub const SQL_CHECKPOINT_DDL: &str = "CREATE TABLE IF NOT EXISTS id_effect_event_checkpoint (
  subscription TEXT PRIMARY KEY,
  position BIGINT NOT NULL
)";

const LOAD_CHECKPOINT_SQL: &str =
  "SELECT position FROM id_effect_event_checkpoint WHERE subscription = $1";

const SAVE_CHECKPOINT_SQL: &str = "INSERT INTO id_effect_event_checkpoint (subscription, position) \
  VALUES ($1, $2) ON CONFLICT (subscription) DO UPDATE SET position = excluded.position";

/// [`CheckpointStore`] in the table of [`SQL_CHECKPOINT_DDL`], through a [`SqlClient`]. Keep it
/// in the database of the read model it guards, so both survive (or are restored) together.
#[derive(Clone)]
pub struct SqlCheckpointStore {
  client: Arc<dyn SqlClient>,
}

impl SqlCheckpointStore {
  /// Checkpoints over `client`.
  pub fn new(client: Arc<dyn SqlClient>) -> Self {
    Self { client }
  }

  /// Checkpoints over the client of a [`SqlClientService`].
  pub fn from_service(service: &SqlClientService) -> Self {
    Self::new(Arc::clone(&service.0))
  }

  /// Run [`SQL_CHECKPOINT_DDL`] (idempotent).
  pub fn create_table(&self) -> Effect<(), EventStoreError> {
    self
      .client
      .execute(SQL_CHECKPOINT_DDL, &[])
      .map(|_| ())
      .map_err(EventStoreError::from)
  }
}

impl CheckpointStore for SqlCheckpointStore {
  fn load(&self, subscription: &str) -> Effect<u64, EventStoreError> {
    self
      .client
      .query(LOAD_CHECKPOINT_SQL, &params![subscription])
      .map_err(EventStoreError::from)
      .flat_map(|rows| {
        ready(match rows.first() {
          Some(row) => row
            .get::<i64>("position")
            .map_err(EventStoreError::from)
            .and_then(to_version),
          None => Ok(0),
        })
      })
  }

  fn save(&self, subscription: &str, position: u64) -> Effect<(), EventStoreError> {
    let position = match to_sql_version(position) {
      Ok(p) => p,
      Err(e) => return fail(e),
    };
    self
      .client
      .execute(SAVE_CHECKPOINT_SQL, &params![subscription, position])
      .map(|_| ())
      .map_err(EventStoreError::from)
  }
}

/// Failure of a [`CatchUpSubscription`] run.
#[derive(Debug, thiserror::Error)]
pub enum SubscriptionError<E: std::error::Error + Send + Sync + 'static> {
  /// The log or the checkpoint store failed.
  #[error("event store: {0}")]
  Store(#[from] EventStoreError),
  /// The handler failed on the event at `position`; the checkpoint stays before it.
  #[error("handler failed at position {position}: {error}")]
  Handler {
    /// Position of the event the handler failed on.
    position: u64,
    /// The handler's error.
    #[source]
    error: E,
  },
}

/// Default number of events read per batch.
pub const DEFAULT_BATCH_SIZE: usize = 100;

/// A named reader of an [`EventLog`] that resumes from its checkpoint in a [`CheckpointStore`].
///
/// [`poll`](Self::poll) handles one batch after the checkpoint; [`catch_up`](Self::catch_up)
/// polls until the log is exhausted. Both are effects: run them at startup and then on a
/// schedule (or after appends) to keep a read model current. Clones share the log and the
/// checkpoints.
///
/// ```rust
/// use id_effect::{Effect, Exit, succeed};
/// use id_effect_events::{
///   CatchUpSubscription, EventStore, MemoryCheckpointStore, MemoryEventStore, RecordedEvent,
/// };
/// use std::convert::Infallible;
/// use std::sync::{Arc, Mutex};
///
/// let store = MemoryEventStore::<i64>::new();
/// let checkpoints = MemoryCheckpointStore::new();
/// let _ = store.append("a", &[5, 7]).block_on();
///
/// let total = Arc::new(Mutex::new(0));
/// let sum = Arc::clone(&total);
/// let add = move |e: &RecordedEvent<i64>| -> Effect<(), Infallible> {
///   *sum.lock().unwrap() += e.event.payload;
///   succeed(())
/// };
/// let totals = CatchUpSubscription::new("totals", store.clone(), checkpoints.clone());
/// assert!(matches!(totals.catch_up(add.clone()).block_on(), Exit::Success(2)));
///
/// // Restarted with the same checkpoints, it only sees what was appended since.
/// let _ = store.append("b", &[1]).block_on();
/// let restarted = CatchUpSubscription::new("totals", store, checkpoints);
/// assert!(matches!(restarted.catch_up(add).block_on(), Exit::Success(1)));
/// assert_eq!(*total.lock().unwrap(), 13);
/// ```
pub struct CatchUpSubscription<E> {
  name: String,
  log: Arc<dyn EventLog<E>>,
  checkpoints: Arc<dyn CheckpointStore>,
  batch_size: usize,
}

impl<E> Clone for CatchUpSubscription<E> {
  fn clone(&self) -> Self {
    Self {
      name: self.name.clone(),
      log: Arc::clone(&self.log),
      checkpoints: Arc::clone(&self.checkpoints),
      batch_size: self.batch_size,
    }
  }
}

/// A handler failure inside a batch: the last position handled before it, and where it failed.
struct Stopped<HE> {
  reached: u64,
  position: u64,
  error: HE,
}

impl<E: Send + Sync + 'static> CatchUpSubscription<E> {
  /// Subscription `name` (its checkpoint key) over `log`, with checkpoints in `checkpoints`,
  /// reading [`DEFAULT_BATCH_SIZE`] events per batch.
  pub fn new(
    name: impl Into<String>,
    log: impl EventLog<E> + 'static,
    checkpoints: impl CheckpointStore + 'static,
  ) -> Self {
    Self {
      name: name.into(),
      log: Arc::new(log),
      checkpoints: Arc::new(checkpoints),
      batch_size: DEFAULT_BATCH_SIZE,
    }
  }

  /// Read `batch_size` events per batch (at least one); the checkpoint is stored per batch.
  pub fn with_batch_size(mut self, batch_size: usize) -> Self {
    self.batch_size = batch_size.max(1);
    self
  }

  /// The subscription's name (its checkpoint key).
  pub fn name(&self) -> &str {
    &self.name
  }

  /// The stored checkpoint: the last position handled (`0`: none yet).
  pub fn checkpoint(&self) -> Effect<u64, EventStoreError> {
    self.checkpoints.load(&self.name)
  }

  /// Handle the next batch after the checkpoint with `handler`, in position order, then store
  /// the last position as the checkpoint. Succeeds with the number of events handled (`0`:
  /// nothing new). When the handler fails, the checkpoint is stored at the event before and
  /// the effect fails with [`SubscriptionError::Handler`].
  pub fn poll<H, HE>(&self, handler: H) -> Effect<usize, SubscriptionError<HE>>
  where
    H: Fn(&RecordedEvent<E>) -> Effect<(), HE> + Send + Sync + 'static,
    HE: std::error::Error + Send + Sync + 'static,
  {
    self.poll_shared(Arc::new(handler))
  }

  /// [`poll`](Self::poll) until a batch comes back short: the subscription has caught up with
  /// the log as it was when the last batch was read. Succeeds with the number of events handled.
  pub fn catch_up<H, HE>(&self, handler: H) -> Effect<usize, SubscriptionError<HE>>
  where
    H: Fn(&RecordedEvent<E>) -> Effect<(), HE> + Send + Sync + 'static,
    HE: std::error::Error + Send + Sync + 'static,
  {
    self.catch_up_from(Arc::new(handler), 0)
  }

  fn catch_up_from<H, HE>(
    &self,
    handler: Arc<H>,
    handled: usize,
  ) -> Effect<usize, SubscriptionError<HE>>
  where
    H: Fn(&RecordedEvent<E>) -> Effect<(), HE> + Send + Sync + 'static,
    HE: std::error::Error + Send + Sync + 'static,
  {
    let this = self.clone();
    self.poll_shared(Arc::clone(&handler)).flat_map(move |n| {
      if n < this.batch_size {
        succeed(handled + n)
      } else {
        this.catch_up_from(handler, handled + n)
      }
    })
  }

  fn poll_shared<H, HE>(&self, handler: Arc<H>) -> Effect<usize, SubscriptionError<HE>>
  where
    H: Fn(&RecordedEvent<E>) -> Effect<(), HE> + Send + Sync + 'static,
    HE: std::error::Error + Send + Sync + 'static,
  {
    let (log, batch_size) = (Arc::clone(&self.log), self.batch_size);
    let (name, checkpoints) = (self.name.clone(), Arc::clone(&self.checkpoints));
    self
      .checkpoints
      .load(&self.name)
      .flat_map(move |after| {
        log
          .read_all(after, batch_size)
          .map(move |events| (after, events))
      })
      .map_err(SubscriptionError::Store)
      .flat_map(move |(after, events)| handle_batch(name, checkpoints, after, events, handler))
  }
}

/// Run `handler` over `events` in order, then store the last position handled.
fn handle_batch<E, H, HE>(
  name: String,
  checkpoints: Arc<dyn CheckpointStore>,
  after: u64,
  events: Vec<RecordedEvent<E>>,
  handler: Arc<H>,
) -> Effect<usize, SubscriptionError<HE>>
where
  E: Send + Sync + 'static,
  H: Fn(&RecordedEvent<E>) -> Effect<(), HE> + Send + Sync + 'static,
  HE: std::error::Error + Send + Sync + 'static,
{
  let count = events.len();
  if count == 0 {
    return succeed(0);
  }
  let mut chain: Effect<u64, Stopped<HE>> = succeed(after);
  for event in events {
    let handler = Arc::clone(&handler);
    chain = chain.flat_map(move |reached| {
      let position = event.position;
      handler(&event)
        .map(move |()| position)
        .map_err(move |error| Stopped {
          reached,
          position,
          error,
        })
    });
  }
  let (stop_name, stop_checkpoints) = (name.clone(), Arc::clone(&checkpoints));
  chain
    .catch(move |stopped| {
      let Stopped {
        reached,
        position,
        error,
      } = stopped;
      let save = if reached > after {
        stop_checkpoints.save(&stop_name, reached)
      } else {
        succeed(())
      };
      save
        .map_err(SubscriptionError::Store)
        .flat_map(move |()| fail(SubscriptionError::Handler { position, error }))
    })
    .flat_map(move |reached| {
      checkpoints
        .save(&name, reached)
        .map(move |()| count)
        .map_err(SubscriptionError::Store)
    })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::edge::run_sync;
  use crate::event_store::{EventStore, MemoryEventStore};
  use id_effect_sql::{SqlRow, SqlValue, TestSqlClient};
  use std::convert::Infallible;

  #[derive(Debug, thiserror::Error)]
  #[error("handler refused {0}")]
  struct Refused(u64);

  /// `(position, stream_id, payload)` of each handled event.
  type Seen = Arc<Mutex<Vec<(u64, String, u32)>>>;

  /// A handler that records the events it saw into `seen`.
  fn recorder(
    seen: &Seen,
  ) -> impl Fn(&RecordedEvent<u32>) -> Effect<(), Infallible> + Send + Sync + 'static {
    let seen = Arc::clone(seen);
    move |e| {
      seen
        .lock()
        .unwrap()
        .push((e.position, e.stream_id.clone(), e.event.payload));
      succeed(())
    }
  }

  #[test]
  fn memory_log_orders_appends_across_streams() {
    let store = MemoryEventStore::<u32>::new();
    run_sync(store.append("a", &[1, 2])).expect("a");
    run_sync(store.append("b", &[3])).expect("b");
    run_sync(store.append("a", &[4])).expect("a again");
    let all = run_sync(store.read_all(0, 10)).expect("read_all");
    let order: Vec<_> = all
      .iter()
      .map(|e| (e.position, e.stream_id.as_str(), e.event.version))
      .collect();
    assert_eq!(order, [(1, "a", 1), (2, "a", 2), (3, "b", 1), (4, "a", 3)]);
    let page = run_sync(Arc::new(store.clone()).read_all(1, 2)).expect("page");
    assert_eq!(
      page.iter().map(|e| e.event.payload).collect::<Vec<_>>(),
      [2, 3]
    );
    assert!(run_sync(store.read_all(4, 10)).expect("end").is_empty());
  }

  #[test]
  fn subscription_resumes_from_checkpoint() {
    let store = MemoryEventStore::<u32>::new();
    let checkpoints = MemoryCheckpointStore::new();
    run_sync(store.append("a", &[1, 2])).expect("a");
    run_sync(store.append("b", &[3])).expect("b");

    let seen = Arc::new(Mutex::new(Vec::new()));
    let first =
      CatchUpSubscription::new("audit", store.clone(), checkpoints.clone()).with_batch_size(2);
    assert_eq!(
      run_sync(first.catch_up(recorder(&seen))).expect("catch up"),
      3
    );
    assert_eq!(run_sync(first.checkpoint()).expect("checkpoint"), 3);
    drop(first);

    // A new process: a fresh subscription over the same log and checkpoints.
    run_sync(store.append("a", &[4])).expect("a again");
    let restarted = CatchUpSubscription::new("audit", store.clone(), checkpoints.clone());
    assert_eq!(restarted.name(), "audit");
    assert_eq!(
      run_sync(restarted.catch_up(recorder(&seen))).expect("resume"),
      1
    );
    assert_eq!(
      *seen.lock().unwrap(),
      [
        (1, "a".to_owned(), 1),
        (2, "a".to_owned(), 2),
        (3, "b".to_owned(), 3),
        (4, "a".to_owned(), 4)
      ]
    );
    assert_eq!(run_sync(restarted.poll(recorder(&seen))).expect("idle"), 0);
    assert_eq!(run_sync(checkpoints.load("audit")).expect("stored"), 4);
    // Another subscription name has its own checkpoint.
    assert_eq!(run_sync(checkpoints.load("other")).expect("other"), 0);
  }

  #[test]
  fn handler_failure_keeps_checkpoint_before_the_failed_event() {
    let store = MemoryEventStore::<u32>::new();
    let checkpoints = MemoryCheckpointStore::new();
    run_sync(store.append("a", &[1, 2, 3])).expect("a");
    let sub = CatchUpSubscription::new("strict", store.clone(), checkpoints.clone());
    let refuse_two = |e: &RecordedEvent<u32>| {
      if e.event.payload == 2 {
        fail(Refused(e.position))
      } else {
        succeed(())
      }
    };
    let err = run_sync(sub.catch_up(refuse_two)).expect_err("refused");
    assert!(matches!(
      err,
      SubscriptionError::Handler {
        position: 2,
        error: Refused(2)
      }
    ));
    assert_eq!(
      err.to_string(),
      "handler failed at position 2: handler refused 2"
    );
    assert_eq!(run_sync(sub.checkpoint()).expect("checkpoint"), 1);

    // Failing on the first event of a batch stores nothing.
    let refuse_all = |e: &RecordedEvent<u32>| -> Effect<(), Refused> { fail(Refused(e.position)) };
    let err = run_sync(sub.poll(refuse_all)).expect_err("refused");
    assert!(matches!(
      err,
      SubscriptionError::Handler { position: 2, .. }
    ));
    assert_eq!(run_sync(sub.checkpoint()).expect("checkpoint"), 1);

    // Once the handler accepts, the subscription continues from the failed event.
    let seen = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(run_sync(sub.catch_up(recorder(&seen))).expect("retry"), 2);
    assert_eq!(seen.lock().unwrap()[0].0, 2);
  }

  #[test]
  fn batch_size_is_at_least_one_and_store_failures_are_typed() {
    let store = MemoryEventStore::<u32>::new();
    run_sync(store.append("a", &[1, 2])).expect("a");
    let sub =
      CatchUpSubscription::new("one", store, MemoryCheckpointStore::new()).with_batch_size(0);
    let seen = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(run_sync(sub.poll(recorder(&seen))).expect("poll"), 1);
    assert_eq!(
      run_sync(sub.clone().poll(recorder(&seen))).expect("poll"),
      1
    );

    let journal = crate::SqlEventJournal::<u32>::new(Arc::new(TestSqlClient::new()));
    let sub = CatchUpSubscription::new("down", journal, MemoryCheckpointStore::new());
    let err = run_sync(sub.poll(recorder(&seen))).expect_err("not connected");
    assert!(matches!(
      err,
      SubscriptionError::Store(EventStoreError::Sql(_))
    ));
    assert!(err.to_string().starts_with("event store: "));
  }

  #[test]
  fn sql_checkpoints_load_and_upsert() {
    let test = TestSqlClient::new();
    run_sync(test.connect()).expect("connect");
    let checkpoints = SqlCheckpointStore::from_service(&SqlClientService::new(test.clone()));
    run_sync(checkpoints.create_table()).expect("ddl");
    assert_eq!(run_sync(checkpoints.load("s")).expect("none"), 0);
    test.script_query(
      LOAD_CHECKPOINT_SQL,
      vec![SqlRow::from_pairs([("position", 9i64)])],
    );
    assert_eq!(run_sync(checkpoints.load("s")).expect("stored"), 9);
    run_sync(Arc::new(checkpoints.clone()).save("s", 12)).expect("save");
    let executes = test.recorded_executes();
    assert_eq!(executes[0].0, SQL_CHECKPOINT_DDL);
    assert_eq!(executes[1].0, SAVE_CHECKPOINT_SQL);
    assert_eq!(
      executes[1].1,
      [SqlValue::Text("s".into()), SqlValue::I64(12)]
    );
    assert!(matches!(
      run_sync(checkpoints.save("s", u64::MAX)).expect_err("range"),
      EventStoreError::Io(_)
    ));
    test.script_query(
      LOAD_CHECKPOINT_SQL,
      vec![SqlRow::from_pairs([("position", -1i64)])],
    );
    assert!(run_sync(Arc::new(checkpoints).load("s")).is_err());
  }
}
