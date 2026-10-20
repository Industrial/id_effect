//! Subscriptions over an `id_effect_events` [`EventLog`] (feature `events`; design note §6).
//!
//! [`EventSubscription`] turns the log into a resumable stream: the cursor of every item is the
//! event's global position (`Cursor::events(pos)`, source 1), and a call that names a cursor
//! replays from the event after it. The same loop reads history and then the live tail — after a
//! read that finds nothing new it waits [`TailConfig::poll_interval`] and reads again from the
//! last position it delivered — so there is no hand-over where an event could be skipped or sent
//! twice. A projection narrows the stream (events it drops still advance the position, so a
//! resume does not see them again either).
//!
//! - **Bounded.** One read returns at most [`TailConfig::batch_size`] events, and the reader only
//!   reads when the transport pulls. A slow subscriber suspends its own reader (the transport's
//!   window is full) and the backlog stays in the log: memory per call is the window plus one
//!   batch. It is never cut off for being slow; it lags and catches up from its position.
//! - **Released on interrupt.** Dropping the call (disconnect, cancel, deadline) interrupts the
//!   fiber that reads, including a reader waiting out its poll interval; nothing keeps polling.
//!   [`EventSubscription::open_readers`] counts the readers still alive.
//! - **Not a capability.** A cursor only names a position. The procedure's guards run on every
//!   subscribe and every resume (each is a call through `dispatch::open`), and the projection is
//!   applied again on resume, so a cursor never widens what a caller sees.
//! - **Faults stay server-side.** A failed read ends the call as a defect: logged with the
//!   correlation id, answered as `InternalError { id }`. The client resumes from its last cursor.
//!
//! Without a cursor a call starts after [`EventSubscription::starting_after`] (default `0`: the
//! whole log).
//!
//! ```rust
//! use id_effect::{DynEnv, Exit, Never, fail, schema, succeed};
//! use id_effect_events::{EventStore, MemoryEventStore};
//! use id_effect_rpc::subscription::events::EventSubscription;
//! use id_effect_rpc::wire::Cursor;
//! use id_effect_rpc::{Procedure, RpcGroup, WireSchema};
//!
//! let store = MemoryEventStore::<i64>::new();
//! let _ = store.append("acct-1", &[5, 7]).block_on();
//! let _ = store.append("acct-2", &[100]).block_on();
//! let _ = store.append("acct-1", &[9]).block_on();
//!
//! // A subscription procedure: the account's amounts, resumable from any cursor.
//! let feed = EventSubscription::new(store.clone());
//! let mut group = RpcGroup::new();
//! let entries = feed.clone();
//! group.subscription(
//!   Procedure::subscription("Ledger.entries").public(),
//!   WireSchema::new(schema::string::<()>()),
//!   WireSchema::new(schema::i64::<()>()),
//!   move |account: String, after: Option<Cursor>, _env: &mut DynEnv| {
//!     match entries.subscribe(after, move |e| (e.stream_id == account).then_some(e.event.payload)) {
//!       Ok(subscription) => succeed(subscription),
//!       Err(mismatch) => fail(mismatch),
//!     }
//!   },
//! );
//!
//! // The stream itself: after position 1, acct-1's entries carry their positions as cursors.
//! let subscription = feed
//!   .subscribe::<_, Never, _>(Some(Cursor::events(1)), |e| {
//!     (e.stream_id == "acct-1").then_some(e.event.payload)
//!   })
//!   .unwrap();
//! let items = subscription.into_stream().take(2).run_collect().provide_all(DynEnv::new());
//! let Exit::Success(items) = items.block_on() else { panic!() };
//! assert_eq!(items[0].value, 7);
//! assert_eq!(items[1].value, 9);
//! assert_eq!(items[1].cursor, Some(Cursor::events(4)));
//! ```

use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use id_effect::{Ctx, DynEnv, Effect, Stream, die, sleep, succeed};
use id_effect_events::{EventLog, EventStoreError, RecordedEvent};

use super::{CursorMismatch, TailConfig, position_after};
use crate::kinds::{Subscription, SubscriptionItem};
use crate::wire::{Cursor, CursorSource};

/// The book chapter on subscriptions runs as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../../../id_effect/book/src/part6/ch33-08-subscriptions.md")]
struct BookSubscriptionsDoctests;

/// A subscription source over an [`EventLog`]: cursor = global position, replay then live tail.
/// Clones share the log, the configuration and the reader count.
pub struct EventSubscription<Ev> {
  log: Arc<dyn EventLog<Ev>>,
  config: TailConfig,
  start: u64,
  readers: Arc<AtomicUsize>,
}

impl<Ev> Clone for EventSubscription<Ev> {
  fn clone(&self) -> Self {
    Self {
      log: Arc::clone(&self.log),
      config: self.config,
      start: self.start,
      readers: Arc::clone(&self.readers),
    }
  }
}

impl<Ev> fmt::Debug for EventSubscription<Ev> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("EventSubscription")
      .field("config", &self.config)
      .field("start", &self.start)
      .field("open_readers", &self.readers.load(Ordering::SeqCst))
      .finish_non_exhaustive()
  }
}

impl<Ev: Send + Sync + 'static> EventSubscription<Ev> {
  /// Subscriptions over `log` ([`TailConfig::default`], starting after position 0).
  pub fn new(log: impl EventLog<Ev> + 'static) -> Self {
    Self::from_shared(Arc::new(log))
  }

  /// Subscriptions over an already shared log.
  pub fn from_shared(log: Arc<dyn EventLog<Ev>>) -> Self {
    Self {
      log,
      config: TailConfig::default(),
      start: 0,
      readers: Arc::new(AtomicUsize::new(0)),
    }
  }

  /// Read with `config` (batch size and poll interval).
  #[must_use]
  pub fn with_config(mut self, config: TailConfig) -> Self {
    self.config = config;
    self
  }

  /// Where a call without a cursor starts: after `position` (`0`, the default: the whole log;
  /// the log's head: only what is appended from now on).
  #[must_use]
  pub fn starting_after(mut self, position: u64) -> Self {
    self.start = position;
    self
  }

  /// The read configuration.
  pub fn config(&self) -> TailConfig {
    self.config
  }

  /// How many readers (streams from [`events`](Self::events) or
  /// [`subscribe`](Self::subscribe)) are alive; a dropped or interrupted call releases its own.
  pub fn open_readers(&self) -> usize {
    self.readers.load(Ordering::SeqCst)
  }

  /// Every event after `after` (or after [`starting_after`](Self::starting_after) without a
  /// cursor), in position order, then every event appended later. The stream never ends and
  /// never fails with `E`; a failed read is a defect.
  ///
  /// # Errors
  ///
  /// [`CursorMismatch`] when `after` is not an events cursor.
  pub fn events<E: Send + 'static>(
    &self,
    after: Option<Cursor>,
  ) -> Result<Stream<RecordedEvent<Ev>, E, DynEnv>, CursorMismatch> {
    self.tail(after, Some)
  }

  /// The subscription of the events `project` keeps, each item carrying its event's position
  /// as its cursor. `project` sees every event in order and may drop it (`None`).
  ///
  /// # Errors
  ///
  /// [`CursorMismatch`] when `after` is not an events cursor.
  pub fn subscribe<O, E, F>(
    &self,
    after: Option<Cursor>,
    mut project: F,
  ) -> Result<Subscription<O, E>, CursorMismatch>
  where
    O: Send + 'static,
    E: Send + 'static,
    F: FnMut(RecordedEvent<Ev>) -> Option<O> + Send + 'static,
  {
    let stream = self.tail(after, move |event| {
      let cursor = Cursor::events(event.position);
      project(event).map(|value| SubscriptionItem::new(value).with_cursor(cursor))
    })?;
    Ok(Subscription::new(stream))
  }

  fn tail<T, E, F>(
    &self,
    after: Option<Cursor>,
    project: F,
  ) -> Result<Stream<T, E, DynEnv>, CursorMismatch>
  where
    T: Send + 'static,
    E: Send + 'static,
    F: FnMut(RecordedEvent<Ev>) -> Option<T> + Send + 'static,
  {
    let from = position_after(after, CursorSource::Events)?.unwrap_or(self.start);
    let reader = Reader {
      log: Arc::clone(&self.log),
      config: self.config,
      position: Arc::new(AtomicU64::new(from)),
      project: Arc::new(Mutex::new(project)),
      _open: OpenReader::new(Arc::clone(&self.readers)),
    };
    Ok(Stream::from_pull(move |_| reader.next()))
  }
}

/// Counts one live reader until dropped (with the stream that owns it).
struct OpenReader(Arc<AtomicUsize>);

impl OpenReader {
  fn new(count: Arc<AtomicUsize>) -> Self {
    count.fetch_add(1, Ordering::SeqCst);
    Self(count)
  }
}

impl Drop for OpenReader {
  fn drop(&mut self) {
    self.0.fetch_sub(1, Ordering::SeqCst);
  }
}

/// The state of one stream: the last position delivered and the projection.
struct Reader<Ev, F> {
  log: Arc<dyn EventLog<Ev>>,
  config: TailConfig,
  position: Arc<AtomicU64>,
  project: Arc<Mutex<F>>,
  _open: OpenReader,
}

impl<Ev, F> Reader<Ev, F>
where
  Ev: Send + Sync + 'static,
{
  /// One read after the last position: the projected events, or (after an empty read and the
  /// poll interval) an empty batch, which the stream pulls again.
  fn next<T, E>(&self) -> Effect<Option<Vec<T>>, E, DynEnv>
  where
    T: Send + 'static,
    E: Send + 'static,
    F: FnMut(RecordedEvent<Ev>) -> Option<T> + Send + 'static,
  {
    let from = self.position.load(Ordering::Acquire);
    let position = Arc::clone(&self.position);
    let project = Arc::clone(&self.project);
    let interval = self.config.poll_interval();
    on_dyn_env(self.log.read_all(from, self.config.batch_size()))
      .catch(move |error| {
        die(format!(
          "subscription: reading the event log after position {from} failed: {error}"
        ))
      })
      .flat_map(move |events| {
        // Only what lies after the last position, in increasing order: a log that answered
        // out of order can never make the stream repeat or rewind.
        let mut last = from;
        let fresh: Vec<RecordedEvent<Ev>> = events
          .into_iter()
          .filter(|event| {
            let newer = event.position > last;
            if newer {
              last = event.position;
            }
            newer
          })
          .collect();
        if fresh.is_empty() {
          return sleep(interval).map(|()| Some(Vec::new()));
        }
        position.store(last, Ordering::Release);
        let mut project = project.lock().unwrap_or_else(PoisonError::into_inner);
        succeed(Some(fresh.into_iter().filter_map(&mut *project).collect()))
      })
  }
}

/// Run a log effect (environment-free) on the subscription's fiber, under its `DynEnv`.
fn on_dyn_env<A: Send + 'static>(
  effect: Effect<A, EventStoreError>,
) -> Effect<A, EventStoreError, DynEnv> {
  Effect::from_ctx(move |ctx: Ctx<DynEnv>| async move { ctx.bind(effect).await })
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{Exit, Never, fail};
  use id_effect_events::{EventStore, MemoryEventStore, StoredEvent};

  fn recorded(position: u64) -> RecordedEvent<i64> {
    RecordedEvent {
      position,
      stream_id: "s".into(),
      event: StoredEvent {
        event_id: position.to_string(),
        version: position,
        payload: i64::try_from(position).unwrap(),
      },
    }
  }

  /// Answers every read with the same events, out of order and repeated.
  struct Unordered;

  impl EventLog<i64> for Unordered {
    fn read_all(&self, after: u64, _: usize) -> Effect<Vec<RecordedEvent<i64>>, EventStoreError> {
      if after >= 3 {
        return fail(EventStoreError::Io("gone".into()));
      }
      succeed(vec![recorded(2), recorded(1), recorded(3), recorded(3)])
    }
  }

  fn collect<T: Send + 'static>(stream: Stream<T, Never, DynEnv>, n: usize) -> Exit<Vec<T>, Never> {
    stream
      .take(n)
      .run_collect()
      .provide_all(DynEnv::new())
      .block_on()
  }

  #[test]
  fn an_out_of_order_log_never_repeats_or_rewinds_and_a_failed_read_is_a_defect() {
    let feed = EventSubscription::new(Unordered);
    let Exit::Success(events) = collect(feed.events(None).unwrap(), 2) else {
      panic!("events")
    };
    let positions: Vec<u64> = events.iter().map(|e| e.position).collect();
    assert_eq!(positions, [2, 3]);
    let Exit::Failure(cause) = collect(feed.events(None).unwrap(), 3) else {
      panic!("the read after position 3 fails")
    };
    assert!(format!("{cause:?}").contains("reading the event log"));
  }

  #[test]
  fn sources_are_configurable_and_count_their_readers() {
    let store = MemoryEventStore::<i64>::new();
    let _ = store.append("a", &[1, 2, 3]).block_on();
    let feed = EventSubscription::from_shared(Arc::new(store))
      .with_config(TailConfig::new().with_batch_size(2))
      .starting_after(1);
    assert_eq!(feed.config().batch_size(), 2);
    let stream = feed.events::<Never>(None).unwrap();
    assert_eq!(feed.open_readers(), 1);
    assert!(format!("{feed:?}").contains("open_readers: 1"));
    let Exit::Success(events) = collect(stream, 2) else {
      panic!("events")
    };
    assert_eq!(events[0].event.payload, 2);
    assert_eq!(feed.clone().open_readers(), 0);
    assert!(
      feed
        .subscribe::<i64, Never, _>(Some(Cursor::sequence(1)), |e| Some(e.event.payload))
        .is_err()
    );
    assert_eq!(feed.open_readers(), 0);
  }
}
