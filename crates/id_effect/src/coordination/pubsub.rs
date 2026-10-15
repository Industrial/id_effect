//! [`PubSub`]: a broadcast hub (Effect.ts `PubSub`). Ported from the v1
//! `coordination::pubsub`, which forwarded a tokio broadcast channel into a queue from a spawned
//! task.
//!
//! Each [`subscribe`](PubSub::subscribe) registers its own [`Queue`] (same strategy and capacity
//! as the hub) and ties its lifetime to a [`Scope`]: when the scope closes, the subscription is
//! removed and its queue shut down. [`publish`](PubSub::publish) offers the message to every
//! subscriber; on a bounded hub it suspends until each subscriber has room (back-pressure), a
//! dropping hub refuses it for full subscribers, a sliding hub evicts their oldest message.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::queue::Queue;
use super::waiters::{Signal, lock};
use crate::failure::Never;
use crate::kernel::{Effect, from_async, suspend, sync};
use crate::scope::Scope;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HubStrategy {
  Bounded,
  Dropping,
  Sliding,
  Unbounded,
}

struct State<A> {
  subs: BTreeMap<u64, Queue<A>>,
  next: u64,
  shutdown: bool,
}

struct Inner<A> {
  state: Mutex<State<A>>,
  strategy: HubStrategy,
  capacity: usize,
  closed: Arc<Signal>,
}

impl<A: Send + 'static> Inner<A> {
  fn new_queue(&self) -> Queue<A> {
    match self.strategy {
      HubStrategy::Bounded => Queue::bounded(self.capacity),
      HubStrategy::Dropping => Queue::dropping(self.capacity),
      HubStrategy::Sliding => Queue::sliding(self.capacity),
      HubStrategy::Unbounded => Queue::unbounded(),
    }
  }

  /// The subscriber queues, or `None` when the hub is shut down.
  fn subscribers(&self) -> Option<Vec<Queue<A>>> {
    let st = lock(&self.state);
    (!st.shutdown).then(|| st.subs.values().cloned().collect())
  }
}

/// A broadcast hub: every subscriber receives every message published after it subscribed.
/// Cloning yields another handle to the same hub.
pub struct PubSub<A> {
  inner: Arc<Inner<A>>,
}

impl<A> Clone for PubSub<A> {
  fn clone(&self) -> Self {
    Self {
      inner: Arc::clone(&self.inner),
    }
  }
}

impl<A> std::fmt::Debug for PubSub<A> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let st = lock(&self.inner.state);
    f.debug_struct("PubSub")
      .field("strategy", &self.inner.strategy)
      .field("subscribers", &st.subs.len())
      .field("shutdown", &st.shutdown)
      .finish()
  }
}

impl<A: Clone + Send + 'static> PubSub<A> {
  fn with(strategy: HubStrategy, capacity: usize) -> Self {
    Self {
      inner: Arc::new(Inner {
        state: Mutex::new(State {
          subs: BTreeMap::new(),
          next: 0,
          shutdown: false,
        }),
        strategy,
        capacity,
        closed: Arc::new(Signal::default()),
      }),
    }
  }

  /// A bounded hub (`capacity` at least 1 per subscriber): publishing suspends until every
  /// subscriber has room.
  pub fn bounded(capacity: usize) -> Self {
    Self::with(HubStrategy::Bounded, capacity.max(1))
  }

  /// An unbounded hub.
  pub fn unbounded() -> Self {
    Self::with(HubStrategy::Unbounded, usize::MAX)
  }

  /// A bounded hub that drops a message for a subscriber whose buffer is full.
  pub fn dropping(capacity: usize) -> Self {
    Self::with(HubStrategy::Dropping, capacity.max(1))
  }

  /// A bounded hub that evicts a slow subscriber's oldest message when its buffer is full.
  pub fn sliding(capacity: usize) -> Self {
    Self::with(HubStrategy::Sliding, capacity.max(1))
  }

  /// The per-subscriber capacity (`usize::MAX` for an unbounded hub).
  pub fn capacity(&self) -> usize {
    self.inner.capacity
  }

  /// `true` once [`shutdown`](Self::shutdown) has run.
  pub fn is_shutdown(&self) -> bool {
    lock(&self.inner.state).shutdown
  }

  /// Publishes `value` to every current subscriber. `true` when every subscriber accepted it;
  /// `false` when the hub is shut down, has no subscriber, or a dropping subscriber was full.
  pub fn publish<E, R>(&self, value: A) -> Effect<bool, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let inner = Arc::clone(&self.inner);
    from_async(move || async move { Ok(publish_to(&inner, value).await) })
  }

  /// Publishes every value in order; returns the values that were not accepted by every
  /// subscriber (see [`publish`](Self::publish)).
  pub fn publish_all<E, R>(&self, values: impl IntoIterator<Item = A>) -> Effect<Vec<A>, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let values: Vec<A> = values.into_iter().collect();
    let inner = Arc::clone(&self.inner);
    from_async(move || async move {
      let mut left = Vec::new();
      for v in values {
        if !publish_to(&inner, v.clone()).await {
          left.push(v);
        }
      }
      Ok(left)
    })
  }

  /// Subscribes: the returned [`Queue`] receives every message published from now on. The
  /// subscription ends when `scope` closes (the queue is shut down, so its takers drain it and
  /// then see `Disconnected`). Subscribing to a shut-down hub yields a shut-down queue.
  pub fn subscribe<E, R>(&self, scope: &Scope) -> Effect<Queue<A>, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let inner = Arc::clone(&self.inner);
    let scope = scope.clone();
    suspend(move || {
      let q = inner.new_queue();
      let id = {
        let mut st = lock(&inner.state);
        if st.shutdown {
          None
        } else {
          let id = st.next;
          st.next += 1;
          st.subs.insert(id, q.clone());
          Some(id)
        }
      };
      if id.is_none() {
        q.shutdown_sync();
      }
      let fin_q = q.clone();
      scope
        .add_finalizer(sync::<(), Never, _>(move || {
          if let Some(id) = id {
            lock(&inner.state).subs.remove(&id);
          }
          fin_q.shutdown_sync();
        }))
        .map(move |()| q)
    })
    .uninterruptible()
  }

  /// Shuts the hub down: later publishes return `false`, every subscriber queue is shut down
  /// (buffered messages stay readable). Idempotent.
  pub fn shutdown<E, R>(&self) -> Effect<(), E, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || {
      let subs = {
        let mut st = lock(&inner.state);
        st.shutdown = true;
        std::mem::take(&mut st.subs)
      };
      for q in subs.into_values() {
        q.shutdown_sync();
      }
      inner.closed.fire();
    })
  }

  /// Suspends until the hub is shut down.
  pub fn await_shutdown<E, R>(&self) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let closed = Arc::clone(&self.inner.closed);
    from_async(move || async move {
      closed.wait().await;
      Ok(())
    })
  }

  /// The largest number of messages buffered for one subscriber (`0` when shut down).
  pub fn size<E, R>(&self) -> Effect<usize, E, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || {
      inner.subscribers().map_or(0, |subs| {
        subs.iter().map(Queue::size_now).max().unwrap_or(0)
      })
    })
  }

  /// `true` when no message is buffered for any subscriber (or the hub is shut down).
  pub fn is_empty<E, R>(&self) -> Effect<bool, E, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || {
      inner
        .subscribers()
        .is_none_or(|subs| subs.iter().all(|q| q.size_now() == 0))
    })
  }

  /// `true` when some subscriber's buffer is full (or the hub is shut down).
  pub fn is_full<E, R>(&self) -> Effect<bool, E, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || {
      inner
        .subscribers()
        .is_none_or(|subs| subs.iter().any(Queue::is_full_now))
    })
  }
}

impl<A> PubSub<A> {
  /// The number of live subscriptions.
  pub fn subscriber_count(&self) -> usize {
    lock(&self.inner.state).subs.len()
  }
}

/// Offers `value` to every subscriber in turn (suspending on bounded ones).
async fn publish_to<A: Clone + Send + 'static>(inner: &Inner<A>, value: A) -> bool {
  let Some(subs) = inner.subscribers() else {
    return false;
  };
  if subs.is_empty() {
    return false;
  }
  let mut all = true;
  for q in subs {
    all &= q.offer_future(value.clone()).await.is_ok();
  }
  all
}
