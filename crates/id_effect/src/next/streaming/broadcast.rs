//! Fan-out: [`Stream::broadcast`] splits one stream into several consumer streams fed by a pump
//! effect, over one sliding [`Queue`] per branch (a slow consumer loses its oldest items instead
//! of holding the others back).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::stream::{FailSlot, Stream, lock, queue_source};
use crate::next::coordination::Queue;
use crate::next::kernel::{Effect, succeed, suspend, sync, yield_fiber};
use crate::runtime::Never;

/// `(consumer streams, pump effect)` from [`Stream::broadcast`].
pub type StreamBroadcastFanout<A, E, R> = (Vec<Stream<A, E, R>>, Effect<(), E, R>);

impl<A, E, R> Stream<A, E, R>
where
  A: Clone + Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Fans this stream out to `branches` consumer streams, each buffering at most `capacity`
  /// (at least 1) items in a sliding queue.
  ///
  /// Returns `(streams, pump)`. Run the pump concurrently with the consumers (for example fork
  /// it): it pulls upstream, publishes every element to every branch (yielding after each so
  /// consumers on the same executor keep up), and ends the branches when the source ends. If the
  /// source fails, the error is stored and the first branch to drain its queue fails with it
  /// (the others end); the pump itself succeeds. With `branches == 0` the pump just drains the
  /// source.
  pub fn broadcast(
    self,
    capacity: usize,
    branches: usize,
  ) -> Effect<StreamBroadcastFanout<A, E, R>, E, R> {
    suspend(move || succeed(fanout(self, capacity.max(1), branches)))
  }

  /// [`broadcast_with_replay`] with a replay length equal to the hub capacity.
  pub fn broadcast_replay(
    self,
    hub_capacity: usize,
    branches: usize,
  ) -> Effect<StreamBroadcastFanout<A, E, R>, E, R> {
    broadcast_with_replay(self, hub_capacity, hub_capacity, branches)
  }
}

/// [`Stream::broadcast`] where each branch retains up to `max(hub_capacity, replay_len)` of the
/// most recent items (its replay tail) until the consumer pulls them. Every branch is subscribed
/// before the pump starts, so each sees the whole stream subject to that retention.
pub fn broadcast_with_replay<A, E, R>(
  stream: Stream<A, E, R>,
  hub_capacity: usize,
  replay_len: usize,
  branches: usize,
) -> Effect<StreamBroadcastFanout<A, E, R>, E, R>
where
  A: Clone + Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  stream.broadcast(hub_capacity.max(replay_len), branches)
}

fn fanout<A, E, R>(
  upstream: Stream<A, E, R>,
  capacity: usize,
  branches: usize,
) -> StreamBroadcastFanout<A, E, R>
where
  A: Clone + Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  if branches == 0 {
    return (Vec::new(), upstream.run_drain());
  }
  let queues: Arc<Vec<Queue<A>>> =
    Arc::new((0..branches).map(|_| Queue::sliding(capacity)).collect());
  let failure: FailSlot<E> = Arc::new(Mutex::new(None));
  let outs = queues
    .iter()
    .map(|q| queue_source(q.clone(), Arc::clone(&failure), VecDeque::new(), |v| v))
    .collect();
  let publish_to = Arc::clone(&queues);
  let close = Arc::clone(&queues);
  let pump = upstream
    .run_for_each_effect(move |a| {
      let mut publish: Effect<(), E, R> = succeed(());
      for q in publish_to.iter() {
        publish = publish.then(q.try_offer(a.clone()).map(|_| ()));
      }
      publish.then(yield_fiber())
    })
    .catch(move |e| {
      *lock(&failure) = Some(e);
      succeed(())
    })
    .ensuring(sync::<(), Never, R>(move || {
      for q in close.iter() {
        q.shutdown_sync();
      }
    }));
  (outs, pump)
}
