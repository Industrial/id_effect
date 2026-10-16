//! Rate operators on the fiber clock: [`Stream::throttle`] (shape: delay to a rate),
//! [`Stream::throttle_enforce`] (drop above a rate) and [`Stream::debounce`] (emit after a quiet
//! period).
//!
//! Time is the pulling fiber's executor clock — `sleep` and `now` of the executor — so on a
//! [`TestExecutor`](crate::TestExecutor) these operators follow its virtual clock and tests drive
//! them with `advance`. A chunk is treated as arriving at one instant.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::pump::{PumpGuard, Recv, feed_pump, fiber_now, recv, start};
use super::stream::{DEFAULT_CHUNK_SIZE, Step, Stream};
use crate::coordination::Queue;
use crate::failure::Never;
use crate::kernel::{Effect, sleep, succeed};

/// A GCRA (generic cell rate algorithm) meter: `units` elements per `per`, with bursts of up to
/// `units` elements. Equivalent to a token bucket of `units` tokens refilled continuously.
#[derive(Clone, Copy)]
struct Rate {
  /// The time one element costs (`per / units`).
  interval: Duration,
  /// How far ahead of the clock the theoretical arrival time may run (`per - interval`).
  tolerance: Duration,
  /// The theoretical arrival time of the next element.
  tat: Option<Instant>,
}

impl Rate {
  fn new(units: usize, per: Duration) -> Self {
    let units = u32::try_from(units.max(1)).unwrap_or(u32::MAX);
    let interval = per / units;
    Self {
      interval,
      tolerance: per.saturating_sub(interval),
      tat: None,
    }
  }

  /// How long until the next element conforms (zero when it conforms at `now`).
  fn wait(&self, now: Instant) -> Duration {
    self.tat.map_or(Duration::ZERO, |tat| {
      tat.saturating_duration_since(now + self.tolerance)
    })
  }

  /// Admits one element at `now` if it conforms, charging its cost.
  fn admit(&mut self, now: Instant) -> bool {
    if !self.wait(now).is_zero() {
      return false;
    }
    self.tat = Some(self.tat.map_or(now, |tat| tat.max(now)) + self.interval);
    true
  }
}

impl<A, E, R> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Delays elements so that at most `units` (at least 1) pass per `per`, with bursts of up to
  /// `units` (Effect.ts `Stream.throttle` with the `"shape"` strategy). Nothing is dropped: a
  /// pull that is over the rate sleeps on the fiber clock until the next element conforms.
  ///
  /// Buffers at most one upstream chunk, and pulls upstream only when it is drained, so a slow
  /// rate backpressures the source.
  ///
  /// ```
  /// use std::sync::Arc;
  /// use std::time::Duration;
  ///
  /// use id_effect::{Stream, TestExecutor};
  ///
  /// let exec = TestExecutor::new();
  /// let h = Stream::<u32>::from_iterable(1..=3)
  ///   .throttle(1, Duration::from_secs(1))
  ///   .run_collect()
  ///   .fork_root(Arc::new(exec.clone()));
  /// exec.run_until_stalled();
  /// assert!(!h.is_done(), "the second element waits one virtual second");
  /// exec.advance(Duration::from_secs(2));
  /// assert!(h.is_done());
  /// ```
  #[must_use]
  pub fn throttle(self, units: usize, per: Duration) -> Self {
    shape_go(Some(self), VecDeque::new(), Rate::new(units, per))
  }

  /// Drops the elements above `units` (at least 1) per `per`, with bursts of up to `units`
  /// (Effect.ts `Stream.throttle` with the `"enforce"` strategy). Never sleeps; the clock is read
  /// once per upstream chunk.
  #[must_use]
  pub fn throttle_enforce(self, units: usize, per: Duration) -> Self {
    enforce_go(self, Rate::new(units, per))
  }

  /// Emits an element only once `d` has passed on the fiber clock without a newer one
  /// (Effect.ts `Stream.debounce`); the pending element is emitted when the source ends, and a
  /// failure is raised at once (the pending element is dropped).
  ///
  /// Upstream is driven by a child fiber of the pulling fiber into a one-chunk buffer, so it
  /// is pulled while the debounce timer runs and backpressured when the consumer is slower.
  /// Dropping the stream (the consumer stopped early) interrupts that fiber: an in-flight
  /// upstream pull is interrupted, its finalizers run and upstream is dropped.
  #[must_use]
  pub fn debounce(self, d: Duration) -> Self {
    let queue = Queue::bounded(1);
    let close = queue.clone();
    let guard = PumpGuard::new(move || close.shutdown_sync());
    let pump = feed_pump(self, &queue, DEFAULT_CHUNK_SIZE);
    debounce_go(Some(pump), queue, guard, d)
  }
}

/// The state machine of [`Stream::throttle`]: upstream's rest, its pulled but not yet emitted
/// items, and the rate meter.
fn shape_go<A, E, R>(
  upstream: Option<Stream<A, E, R>>,
  buf: VecDeque<A>,
  rate: Rate,
) -> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Stream::from_step(move |n| {
    if !buf.is_empty() {
      return shape_emit(upstream, buf, rate, n);
    }
    match upstream {
      None => succeed(None),
      Some(s) => s.pull_raw(n).flat_map(move |step| match step {
        None => succeed(None),
        Some((items, rest)) if items.is_empty() => {
          succeed(Some((items, shape_go(Some(rest), buf, rate))))
        }
        Some((items, rest)) => shape_emit(Some(rest), items.into(), rate, n),
      }),
    }
  })
}

/// Emits the buffered items that conform now, sleeping first until at least one does.
fn shape_emit<A, E, R>(
  upstream: Option<Stream<A, E, R>>,
  buf: VecDeque<A>,
  rate: Rate,
  n: usize,
) -> Effect<Step<A, E, R>, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  let emit = move |now: Instant| {
    let (mut buf, mut rate) = (buf, rate);
    let mut out = Vec::new();
    while out.len() < n && !buf.is_empty() && rate.admit(now) {
      out.extend(buf.pop_front());
    }
    Some((out, shape_go(upstream, buf, rate)))
  };
  fiber_now().flat_map(move |now| {
    let wait = rate.wait(now);
    if wait.is_zero() {
      succeed(emit(now))
    } else {
      sleep(wait).flat_map(move |()| fiber_now().map(emit))
    }
  })
}

/// The state machine of [`Stream::throttle_enforce`].
fn enforce_go<A, E, R>(upstream: Stream<A, E, R>, rate: Rate) -> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Stream::from_step(move |n| {
    upstream.pull_raw(n).flat_map(move |step| match step {
      None => succeed(None),
      Some((items, rest)) => fiber_now().map(move |now| {
        let mut rate = rate;
        let out: Vec<A> = items.into_iter().filter(|_| rate.admit(now)).collect();
        Some((out, enforce_go(rest, rate)))
      }),
    })
  })
}

/// The state machine of [`Stream::debounce`]: the pump (until started), its queue and guard.
fn debounce_go<A, E, R>(
  pump: Option<Effect<(), Never, R>>,
  queue: Queue<Result<Vec<A>, E>>,
  guard: Arc<PumpGuard>,
  d: Duration,
) -> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Stream::from_step(move |_| {
    start(pump, &guard).flat_map(move |()| debounce_wait(queue, guard, d, None))
  })
}

/// Waits for the next element to settle: `pending` is the latest element and its deadline.
fn debounce_wait<A, E, R>(
  queue: Queue<Result<Vec<A>, E>>,
  guard: Arc<PumpGuard>,
  d: Duration,
  pending: Option<(A, Instant)>,
) -> Effect<Step<A, E, R>, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  let deadline = pending.as_ref().map(|(_, at)| *at);
  recv(queue.clone(), deadline).flat_map(move |(got, now)| match (got, pending) {
    (Recv::Chunk(items), pending) => {
      let pending = items.into_iter().last().map(|a| (a, now + d)).or(pending);
      debounce_wait(queue, guard, d, pending)
    }
    (Recv::Timeout, Some((a, _))) => succeed(Some((vec![a], debounce_go(None, queue, guard, d)))),
    (Recv::Timeout, None) => debounce_wait(queue, guard, d, None),
    (Recv::End, pending) => succeed(pending.map(|(a, _)| (vec![a], Stream::empty()))),
  })
}
