//! The inbox: idempotent consumers. [`Inbox::once`] records `(consumer group, message id)` in the
//! table `id_effect_inbox` with the handler's [`SqlTransaction`] and runs the handler only the
//! first time, so a message the broker redelivers (or the relay publishes twice) is processed
//! once; [`Inbox::handler`] wraps it into a [`consume`](crate::consume) handler that acks after
//! the commit. [`MemoryInbox`] is the same in memory.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use id_effect::{Effect, Semaphore, WallClock, succeed, suspend};
use id_effect_sql::{SqlClient, SqlError, SqlTransaction, params, with_transaction};

use crate::broker::{BrokerError, ConsumerGroup, Message, MessageId};
use crate::delivery::{Delivery, Nack};
use crate::outbox::earlier;

/// Record the message; a conflict inserts nothing and never aborts the transaction.
pub(crate) const MARK: &str = "INSERT INTO id_effect_inbox (consumer_group, message_id, \
  processed_at) VALUES ($1, $2, $3) ON CONFLICT (consumer_group, message_id) DO NOTHING";

pub(crate) const PRUNE: &str = "DELETE FROM id_effect_inbox WHERE processed_at < $1";

/// What [`Inbox::once`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Once<A> {
  /// First time: the handler ran, with this result.
  Ran(A),
  /// The group processed the message before: the handler did not run.
  Duplicate,
}

impl<A> Once<A> {
  /// Whether the handler was skipped.
  pub fn is_duplicate(&self) -> bool {
    matches!(self, Once::Duplicate)
  }

  /// The handler's result, if it ran.
  pub fn ran(self) -> Option<A> {
    match self {
      Once::Ran(a) => Some(a),
      Once::Duplicate => None,
    }
  }
}

/// Settles `delivery` after its handler: ack when it succeeded (or was a duplicate), requeue when
/// it failed.
fn settle(delivery: Delivery, processed: bool) -> Effect<(), BrokerError> {
  if processed {
    delivery.ack()
  } else {
    delivery.nack(Nack::Requeue)
  }
}

/// Deduplicates consumers in PostgreSQL (the table `id_effect_inbox`, from
/// [`migrator`](crate::migrator)). Cheap to clone; it holds only the [`WallClock`] of the
/// `processed_at` times.
#[derive(Clone, Debug)]
pub struct Inbox {
  clock: WallClock,
}

impl Default for Inbox {
  fn default() -> Self {
    Self::new()
  }
}

impl Inbox {
  /// An inbox on the system clock.
  pub fn new() -> Self {
    Self {
      clock: WallClock::system(),
    }
  }

  /// Reads `processed_at` from `clock`.
  #[must_use]
  pub fn with_clock(mut self, clock: WallClock) -> Self {
    self.clock = clock;
    self
  }

  /// Runs `f` with `tx` unless `group` already processed message `id`: inserts
  /// `(group, id)` into `id_effect_inbox` with `tx` (`ON CONFLICT DO NOTHING`), then runs `f`
  /// when the row is new ([`Once::Ran`]) and skips it when it exists ([`Once::Duplicate`]).
  ///
  /// The record commits or rolls back with `tx`: commit when `f` succeeded, roll back when it
  /// failed (as [`with_transaction`] does), so a failed message is processed again on its
  /// redelivery. A concurrent delivery of the same message waits on the uncommitted row and then
  /// sees a duplicate (or, if the first rolled back, runs `f` itself).
  pub fn once<A, E, F>(
    &self,
    tx: &Arc<dyn SqlTransaction>,
    group: &ConsumerGroup,
    id: &MessageId,
    f: F,
  ) -> Effect<Once<A>, E>
  where
    A: Send + 'static,
    E: From<SqlError> + Send + 'static,
    F: FnOnce(Arc<dyn SqlTransaction>) -> Effect<A, E> + Send + 'static,
  {
    let tx = Arc::clone(tx);
    let (group, id) = (group.as_str().to_owned(), id.as_str().to_owned());
    self.clock.now().flat_map(move |now| {
      tx.execute(MARK, &params![group, id, now.0])
        .map_err(E::from)
        .flat_map(move |inserted| {
          if inserted == 0 {
            succeed(Once::Duplicate)
          } else {
            f(tx).map(Once::Ran)
          }
        })
    })
  }

  /// A [`consume`](crate::consume) handler: for each delivery, begins a transaction on
  /// `client`, runs `handler` with it through [`once`](Self::once) on the delivery's group and
  /// message id, commits, and then acks (a duplicate is acked without running `handler`). When
  /// `handler` (or the transaction) fails, the transaction rolls back and the delivery is
  /// requeued ([`Nack::Requeue`]), so it is dead-lettered after
  /// [`max_deliveries`](crate::SubscribeOptions::max_deliveries); the error is dropped. An ack
  /// lost after the commit is harmless: the redelivery is a duplicate.
  pub fn handler<E, F>(
    &self,
    client: Arc<dyn SqlClient>,
    handler: F,
  ) -> impl Fn(Delivery) -> Effect<(), BrokerError> + Send + Sync + 'static
  where
    E: From<SqlError> + Send + 'static,
    F: Fn(Arc<dyn SqlTransaction>, &Message) -> Effect<(), E> + Send + Sync + 'static,
  {
    let inbox = self.clone();
    let handler = Arc::new(handler);
    move |delivery: Delivery| {
      let (inbox, client, handler) = (inbox.clone(), Arc::clone(&client), Arc::clone(&handler));
      // `consume` builds a chunk's effects before running them: begin when this runs.
      suspend(move || {
        let message = delivery.message().clone();
        let group = delivery.group().clone();
        with_transaction(client, move |tx: Arc<dyn SqlTransaction>| {
          let id = message.id().clone();
          inbox.once(&tx, &group, &id, move |tx| handler(tx, &message))
        })
        .map(|_: Once<()>| true)
        .catch(|_: E| succeed(false))
        .flat_map(move |processed| settle(delivery, processed))
      })
    }
  }

  /// Deletes the inbox rows processed more than `keep` ago. A message redelivered after its row
  /// is pruned is processed again: keep rows well beyond the longest redelivery (and outbox
  /// retry) horizon.
  pub fn prune(&self, client: &Arc<dyn SqlClient>, keep: Duration) -> Effect<u64, SqlError> {
    let client = Arc::clone(client);
    self
      .clock
      .now()
      .flat_map(move |now| client.execute(PRUNE, &params![earlier(now, keep).0]))
  }
}

#[derive(Default)]
struct MemoryState {
  done: HashSet<(ConsumerGroup, MessageId)>,
  locks: HashMap<(ConsumerGroup, MessageId), Semaphore>,
}

/// [`Inbox`] in memory, for tests and single-process consumers: the same once-only processing
/// without a database. A message is recorded when its handler succeeds; concurrent calls for
/// the same message run one at a time, so the second sees the first's outcome. Records are kept
/// until the inbox is dropped. Clones share them.
#[derive(Clone, Default)]
pub struct MemoryInbox {
  state: Arc<Mutex<MemoryState>>,
}

impl fmt::Debug for MemoryInbox {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MemoryInbox")
      .field("processed", &self.len())
      .finish()
  }
}

impl MemoryInbox {
  /// An empty inbox.
  pub fn new() -> Self {
    Self::default()
  }

  fn lock(&self) -> MutexGuard<'_, MemoryState> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// How many messages were processed, over every group.
  pub fn len(&self) -> usize {
    self.lock().done.len()
  }

  /// Whether no message was processed.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Whether `group` processed message `id`.
  pub fn contains(&self, group: &ConsumerGroup, id: &MessageId) -> bool {
    self.lock().done.contains(&(group.clone(), id.clone()))
  }

  /// Runs `f` unless `group` already processed message `id`; records it when `f` succeeds.
  pub fn once<A, E, F>(&self, group: &ConsumerGroup, id: &MessageId, f: F) -> Effect<Once<A>, E>
  where
    A: Send + 'static,
    E: Send + 'static,
    F: FnOnce() -> Effect<A, E> + Send + 'static,
  {
    let key = (group.clone(), id.clone());
    let gate = self
      .lock()
      .locks
      .entry(key.clone())
      .or_insert_with(|| Semaphore::new(1))
      .clone();
    let state = Arc::clone(&self.state);
    gate.with_permit(suspend(move || {
      let seen = state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .done
        .contains(&key);
      if seen {
        return succeed(Once::Duplicate);
      }
      f().map(move |a| {
        state
          .lock()
          .unwrap_or_else(PoisonError::into_inner)
          .done
          .insert(key);
        Once::Ran(a)
      })
    }))
  }

  /// A [`consume`](crate::consume) handler: runs `handler` through [`once`](Self::once) on each
  /// delivery's group and message id, then acks (a duplicate without running it); a failure
  /// requeues the delivery and drops the error.
  pub fn handler<E, F>(
    &self,
    handler: F,
  ) -> impl Fn(Delivery) -> Effect<(), BrokerError> + Send + Sync + 'static
  where
    E: Send + 'static,
    F: Fn(&Message) -> Effect<(), E> + Send + Sync + 'static,
  {
    let inbox = self.clone();
    let handler = Arc::new(handler);
    move |delivery: Delivery| {
      let (inbox, handler) = (inbox.clone(), Arc::clone(&handler));
      suspend(move || {
        let message = delivery.message().clone();
        let id = message.id().clone();
        inbox
          .once(delivery.group(), &id, move || handler(&message))
          .map(|_| true)
          .catch(|_: E| succeed(false))
          .flat_map(move |processed| settle(delivery, processed))
      })
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicUsize, Ordering};

  use id_effect::{Cause, Exit, fail};
  use id_effect_sql::{SqlValue, TestSqlClient};

  use super::*;
  use crate::{MemoryBroker, MessageBroker, SubscribeOptions, Topic};

  fn run<A: Send + 'static, E: fmt::Debug + Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn group() -> ConsumerGroup {
    ConsumerGroup::new("billing").unwrap()
  }

  #[test]
  fn once_runs_on_a_new_row_and_skips_a_duplicate() {
    let client = TestSqlClient::new();
    run(client.connect()).unwrap();
    let inbox = Inbox::default();
    let tx = run(client.begin()).unwrap();
    let id = MessageId::new("m-1");
    // TestSqlClient answers 0 rows affected unless scripted: a duplicate.
    let dup = run(inbox.once(&tx, &group(), &id, |_| succeed::<_, SqlError, _>(1)));
    assert_eq!(dup, Ok(Once::Duplicate));
    client.script_execute(MARK, 1);
    let ran = run(inbox.once(&tx, &group(), &id, |_| succeed::<_, SqlError, _>(7)));
    assert_eq!(ran, Ok(Once::Ran(7)));
    run(tx.commit()).unwrap();
    let (sql, params) = &client.recorded_executes()[0];
    assert_eq!(sql, MARK);
    assert_eq!(params[0], SqlValue::Text("billing".into()));
    assert_eq!(params[1], SqlValue::Text("m-1".into()));
    assert_eq!(Once::Ran(3).ran(), Some(3));
    assert!(Once::<()>::Duplicate.is_duplicate() && Once::<()>::Duplicate.ran().is_none());
    client.script_execute(PRUNE, 2);
    let client: Arc<dyn SqlClient> = Arc::new(client);
    assert_eq!(run(inbox.prune(&client, Duration::from_secs(60))), Ok(2));
  }

  #[test]
  fn memory_inbox_records_only_successes() {
    let inbox = MemoryInbox::new();
    let id = MessageId::new("m-1");
    let failed = run(inbox.once(&group(), &id, || fail::<(), _, _>("boom")));
    assert_eq!(failed, Err("boom"));
    assert!(inbox.is_empty(), "a failure is not recorded");
    assert_eq!(
      run(inbox.once(&group(), &id, || succeed::<_, (), _>(1))),
      Ok(Once::Ran(1))
    );
    assert_eq!(
      run(inbox.once(&group(), &id, || succeed::<_, (), _>(2))),
      Ok(Once::Duplicate)
    );
    let other = ConsumerGroup::new("audit").unwrap();
    assert_eq!(
      run(inbox.once(&other, &id, || succeed::<_, (), _>(3))),
      Ok(Once::Ran(3))
    );
    assert!(inbox.contains(&group(), &id) && inbox.contains(&other, &id));
    assert_eq!(format!("{inbox:?}"), "MemoryInbox { processed: 2 }");
  }

  #[test]
  fn memory_handler_processes_redeliveries_once() {
    let broker = MemoryBroker::new();
    let orders = Topic::new("orders").unwrap();
    let inbox = MemoryInbox::new();
    let runs = Arc::new(AtomicUsize::new(0));
    // The same message published twice (a relay that crashed after publishing), then a fresh one
    // whose first attempt fails.
    let m = Message::new("a").with_id("m-1");
    for msg in [m.clone(), m, Message::new("b").with_id("m-2")] {
      run(broker.publish(&orders, msg)).unwrap();
    }
    let handler = inbox.handler({
      let runs = Arc::clone(&runs);
      move |msg: &Message| {
        let n = runs.fetch_add(1, Ordering::SeqCst);
        if msg.id().as_str() == "m-2" && n == 1 {
          fail("first attempt of m-2 fails")
        } else {
          succeed(())
        }
      }
    });
    let opts = SubscribeOptions::default().prefetch(1);
    let consumed = broker
      .subscribe(&orders, &group(), opts)
      .flat_map(move |deliveries| deliveries.take(4).run_for_each(handler));
    run(consumed).unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 3, "m-1 once, m-2 twice");
    assert_eq!(inbox.len(), 2);
    assert_eq!(broker.backlog(&orders, &group()), 0);
    assert_eq!(broker.in_flight(&orders, &group()), 0);
  }
}
