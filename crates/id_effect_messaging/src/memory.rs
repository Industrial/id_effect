//! [`MemoryBroker`]: every [`MessageBroker`] semantic in one process — consumer groups, acks,
//! redelivery, dead-lettering and prefetch — for tests and single-process development.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use id_effect::{Deferred, Effect, Never, Stream, fail, succeed, suspend};

use crate::Topic;
use crate::broker::{
  BrokerError, ConsumerGroup, Message, MessageBroker, StartFrom, SubscribeOptions,
};
use crate::delivery::{Acker, DeadLetterReason, Delivery, Nack};

/// One consumer group's position in a topic, the model of a Redis Streams group: a cursor over
/// the log, the unsettled entries with their delivery counts (the pending entries list), and
/// the entries waiting to be redelivered.
#[derive(Default)]
struct Group {
  cursor: usize,
  unsettled: BTreeMap<usize, u32>,
  redeliver: VecDeque<usize>,
}

#[derive(Default)]
struct Log {
  messages: Vec<Message>,
  groups: BTreeMap<ConsumerGroup, Group>,
}

struct State {
  topics: BTreeMap<Topic, Log>,
  outage: Option<String>,
  closed: bool,
  /// Completed (and replaced) whenever a pull may now succeed.
  waiters: Deferred<(), Never>,
  /// The waiters to wake once the lock is released.
  wake: Option<Deferred<(), Never>>,
}

impl Default for State {
  fn default() -> Self {
    Self {
      topics: BTreeMap::new(),
      outage: None,
      closed: false,
      waiters: Deferred::new(),
      wake: None,
    }
  }
}

impl State {
  /// Wakes the waiting pulls when the lock is released ([`unlock`]).
  fn notify(&mut self) {
    if self.wake.is_none() {
      self.wake = Some(std::mem::take(&mut self.waiters));
    }
  }

  fn append(&mut self, topic: &Topic, message: Message) {
    self
      .topics
      .entry(topic.clone())
      .or_default()
      .messages
      .push(message);
    self.notify();
  }

  /// Hands out up to `want` entries of the subscription's group: redeliveries first, then new
  /// messages; each with its delivery count, counted now.
  fn claim(&mut self, sub: &Sub, want: usize) -> Vec<(usize, Message, u32)> {
    let mut out = Vec::new();
    let Some(log) = self.topics.get_mut(&sub.topic) else {
      return out;
    };
    let Some(group) = log.groups.get_mut(&sub.group) else {
      return out;
    };
    while out.len() < want {
      let seq = if let Some(seq) = group.redeliver.pop_front() {
        seq
      } else if group.cursor < log.messages.len() {
        group.cursor += 1;
        group.cursor - 1
      } else {
        break;
      };
      let count = group.unsettled.entry(seq).or_insert(0);
      *count = count.saturating_add(1);
      out.push((seq, log.messages[seq].clone(), *count));
    }
    out
  }

  /// Applies a settlement of entry `seq` of the subscription's group.
  fn settle(&mut self, sub: &Sub, seq: usize, outcome: Outcome) {
    let Some(log) = self.topics.get_mut(&sub.topic) else {
      return;
    };
    let Some(group) = log.groups.get_mut(&sub.group) else {
      return;
    };
    let Some(&count) = group.unsettled.get(&seq) else {
      return;
    };
    let dead = match outcome {
      Outcome::Ack => None,
      Outcome::Nack(Nack::DeadLetter) => Some(DeadLetterReason::Rejected),
      Outcome::Nack(Nack::Requeue) | Outcome::Abandon if count >= sub.max_deliveries => {
        Some(DeadLetterReason::MaxDeliveries)
      }
      Outcome::Nack(Nack::Requeue) | Outcome::Abandon => {
        group.redeliver.push_back(seq);
        self.notify();
        return;
      }
    };
    group.unsettled.remove(&seq);
    if let Some(reason) = dead {
      let message = log.messages[seq]
        .clone()
        .dead_lettered(&sub.topic, &sub.group, count, reason);
      self.append(&sub.dead_letter, message);
    }
  }
}

/// How a delivery was settled.
#[derive(Clone, Copy)]
enum Outcome {
  Ack,
  Nack(Nack),
  /// Dropped unsettled: a [`Nack::Requeue`].
  Abandon,
}

type Shared = Arc<Mutex<State>>;

fn lock(shared: &Shared) -> MutexGuard<'_, State> {
  shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Releases `st`, then wakes the pulls a change under it notified.
fn unlock(mut st: MutexGuard<'_, State>) {
  let wake = st.wake.take();
  drop(st);
  if let Some(waiters) = wake {
    waiters.try_succeed(());
  }
}

/// One subscription: what its deliveries need to settle, and its unsettled count.
struct Sub {
  topic: Topic,
  group: ConsumerGroup,
  prefetch: usize,
  max_deliveries: u32,
  dead_letter: Topic,
  /// Deliveries handed out and not settled yet; changed under the state lock.
  in_flight: AtomicUsize,
}

/// The [`Acker`] of a memory delivery. Dropped unsettled, it requeues the delivery.
struct MemoryAcker {
  shared: Shared,
  sub: Arc<Sub>,
  seq: usize,
  done: bool,
}

impl MemoryAcker {
  fn finish(&mut self, outcome: Outcome) {
    if self.done {
      return;
    }
    self.done = true;
    let mut st = lock(&self.shared);
    st.settle(&self.sub, self.seq, outcome);
    // Before the wake-up, so a woken pull sees the freed credit.
    self.sub.in_flight.fetch_sub(1, Ordering::SeqCst);
    st.notify();
    unlock(st);
  }

  /// Settles with `outcome` when the effect runs. During an outage it fails instead and the
  /// delivery is requeued, like an ack lost on the wire.
  fn settle(self: Box<Self>, outcome: Outcome) -> Effect<(), BrokerError> {
    suspend(move || {
      let mut me = self;
      let outage = lock(&me.shared).outage.clone();
      match outage {
        Some(m) => fail(BrokerError::Connection(m)),
        None => {
          me.finish(outcome);
          succeed(())
        }
      }
    })
  }
}

impl Drop for MemoryAcker {
  fn drop(&mut self) {
    self.finish(Outcome::Abandon);
  }
}

impl Acker for MemoryAcker {
  fn ack(self: Box<Self>) -> Effect<(), BrokerError> {
    self.settle(Outcome::Ack)
  }

  fn nack(self: Box<Self>, nack: Nack) -> Effect<(), BrokerError> {
    self.settle(Outcome::Nack(nack))
  }
}

/// One pull of a subscription: up to `max` deliveries within its prefetch credit, waiting until
/// there is at least one; the end once the broker is closed.
fn pull(shared: Shared, sub: Arc<Sub>, max: usize) -> Effect<Option<Vec<Delivery>>, BrokerError> {
  suspend(move || {
    let mut st = lock(&shared);
    if let Some(m) = st.outage.clone() {
      return fail(BrokerError::Connection(m));
    }
    if st.closed {
      return succeed(None);
    }
    let credit = sub
      .prefetch
      .saturating_sub(sub.in_flight.load(Ordering::SeqCst));
    let claimed = st.claim(&sub, credit.min(max));
    if claimed.is_empty() {
      let waiters = st.waiters.clone();
      drop(st);
      return waiters
        .wait()
        .map_err(|never: Never| match never {})
        .flat_map(move |()| pull(shared, sub, max));
    }
    sub.in_flight.fetch_add(claimed.len(), Ordering::SeqCst);
    drop(st);
    let deliveries = claimed
      .into_iter()
      .map(|(seq, message, attempt)| {
        let acker = MemoryAcker {
          shared: Arc::clone(&shared),
          sub: Arc::clone(&sub),
          seq,
          done: false,
        };
        Delivery::new(
          message,
          sub.topic.clone(),
          sub.group.clone(),
          attempt,
          Box::new(acker),
        )
      })
      .collect();
    succeed(Some(deliveries))
  })
}

/// A [`MessageBroker`] in memory. Clones share the topics.
///
/// Each topic is an append-only log; each consumer group reads it from its own cursor and keeps
/// its unsettled deliveries with their delivery counts, as Redis Streams does. A nack with
/// [`Nack::Requeue`], a failed or dropped settlement, or a delivery dropped unsettled queues the
/// message for redelivery to the group, ahead of new messages; on its
/// [`max_deliveries`](SubscribeOptions::max_deliveries)-th delivery it is published to the
/// dead-letter topic instead ([`Message::dead_lettered`]), as is a [`Nack::DeadLetter`]. Messages
/// are kept until the broker is dropped.
#[derive(Clone, Default)]
pub struct MemoryBroker {
  shared: Shared,
}

impl fmt::Debug for MemoryBroker {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let st = lock(&self.shared);
    f.debug_struct("MemoryBroker")
      .field("topics", &st.topics.len())
      .field("closed", &st.closed)
      .finish()
  }
}

impl MemoryBroker {
  /// A broker with no topics.
  pub fn new() -> Self {
    Self::default()
  }

  /// Every message published to `topic` so far, in order (dead-lettered ones included on a
  /// dead-letter topic).
  pub fn messages(&self, topic: &Topic) -> Vec<Message> {
    lock(&self.shared)
      .topics
      .get(topic)
      .map(|log| log.messages.clone())
      .unwrap_or_default()
  }

  /// The deliveries of `group` on `topic` handed out and not settled yet.
  pub fn in_flight(&self, topic: &Topic, group: &ConsumerGroup) -> usize {
    self.group(topic, group, |g, _| g.unsettled.len() - g.redeliver.len())
  }

  /// The messages of `group` on `topic` waiting to be delivered: new ones and redeliveries.
  pub fn backlog(&self, topic: &Topic, group: &ConsumerGroup) -> usize {
    self.group(topic, group, |g, len| len - g.cursor + g.redeliver.len())
  }

  fn group(
    &self,
    topic: &Topic,
    group: &ConsumerGroup,
    f: impl Fn(&Group, usize) -> usize,
  ) -> usize {
    let st = lock(&self.shared);
    st.topics
      .get(topic)
      .and_then(|log| log.groups.get(group).map(|g| f(g, log.messages.len())))
      .unwrap_or(0)
  }

  /// Make every later publish, subscribe, pull and settlement fail with
  /// [`BrokerError::Connection`]`(message)` (an outage; a pull that fails ends its stream), or
  /// recover with `None`.
  pub fn fail_with(&self, message: Option<&str>) {
    lock(&self.shared).outage = message.map(str::to_owned);
  }

  /// Shuts the broker down: publish and subscribe fail with [`BrokerError::Closed`] and every
  /// subscription stream ends at its next pull (its unsettled deliveries can still be settled).
  pub fn close(&self) {
    let mut st = lock(&self.shared);
    st.closed = true;
    st.notify();
    unlock(st);
  }

  /// A step over the state, failing during an outage or once closed.
  fn with<A: Send + 'static>(
    &self,
    f: impl FnOnce(&mut State) -> Result<A, BrokerError> + Send + 'static,
  ) -> Effect<A, BrokerError> {
    let shared = Arc::clone(&self.shared);
    suspend(move || {
      let mut st = lock(&shared);
      if let Some(m) = st.outage.clone() {
        return fail(BrokerError::Connection(m));
      }
      if st.closed {
        return fail(BrokerError::Closed);
      }
      let out = f(&mut st);
      unlock(st);
      match out {
        Ok(a) => succeed(a),
        Err(e) => fail(e),
      }
    })
  }
}

impl MessageBroker for MemoryBroker {
  fn publish(&self, topic: &Topic, message: Message) -> Effect<(), BrokerError> {
    let topic = topic.clone();
    self.with(move |st| {
      st.append(&topic, message);
      Ok(())
    })
  }

  fn subscribe(
    &self,
    topic: &Topic,
    group: &ConsumerGroup,
    options: SubscribeOptions,
  ) -> Effect<Stream<Delivery, BrokerError>, BrokerError> {
    let (topic, group) = (topic.clone(), group.clone());
    let shared = Arc::clone(&self.shared);
    self
      .with(move |st| {
        let dead_letter = options.validate(&topic)?;
        let log = st.topics.entry(topic.clone()).or_default();
        let len = log.messages.len();
        log.groups.entry(group.clone()).or_insert_with(|| Group {
          cursor: match options.start {
            StartFrom::Earliest => 0,
            StartFrom::Latest => len,
          },
          ..Group::default()
        });
        Ok(Arc::new(Sub {
          topic,
          group,
          prefetch: options.prefetch,
          max_deliveries: options.max_deliveries,
          dead_letter,
          in_flight: AtomicUsize::new(0),
        }))
      })
      .map(move |sub| {
        Stream::from_pull(move |max| pull(Arc::clone(&shared), Arc::clone(&sub), max))
      })
  }
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit};

  use super::*;

  fn run<A: Send + 'static>(eff: Effect<A, BrokerError>) -> Result<A, BrokerError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn names() -> (Topic, ConsumerGroup) {
    (
      Topic::new("orders").unwrap(),
      ConsumerGroup::new("billing").unwrap(),
    )
  }

  #[test]
  fn unknown_topics_and_groups_are_empty() {
    let broker = MemoryBroker::new();
    let (t, g) = names();
    assert!(broker.messages(&t).is_empty());
    assert_eq!((broker.in_flight(&t, &g), broker.backlog(&t, &g)), (0, 0));
    assert!(format!("{broker:?}").contains("topics: 0"));
  }

  #[test]
  fn a_settlement_during_an_outage_fails_and_requeues() {
    let broker = MemoryBroker::new();
    let (t, g) = names();
    run(broker.publish(&t, Message::new("a"))).unwrap();
    let mut deliveries = run(
      broker
        .subscribe(&t, &g, SubscribeOptions::default())
        .flat_map(|s| s.take(1).run_collect()),
    )
    .unwrap();
    broker.fail_with(Some("down"));
    let ack = deliveries.pop().unwrap().ack();
    assert_eq!(run(ack), Err(BrokerError::Connection("down".into())));
    broker.fail_with(None);
    assert_eq!((broker.in_flight(&t, &g), broker.backlog(&t, &g)), (0, 1));
  }

  #[test]
  fn an_unrun_settlement_requeues_when_dropped() {
    let broker = MemoryBroker::new();
    let (t, g) = names();
    run(broker.publish(&t, Message::new("a"))).unwrap();
    let mut deliveries = run(
      broker
        .subscribe(&t, &g, SubscribeOptions::default())
        .flat_map(|s| s.take(1).run_collect()),
    )
    .unwrap();
    assert_eq!(broker.in_flight(&t, &g), 1);
    drop(deliveries.pop().unwrap().ack());
    assert_eq!((broker.in_flight(&t, &g), broker.backlog(&t, &g)), (0, 1));
  }
}
