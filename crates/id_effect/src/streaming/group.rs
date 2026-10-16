//! Grouping by key: [`Stream::group_by_key`] splits a stream into one substream per key, and
//! [`Stream::group_by`] runs a stream per group concurrently and merges their outputs (Effect.ts
//! `Stream.groupByKey` and `GroupBy.evaluate`).
//!
//! A router fiber (see `pump.rs`) pulls upstream and offers each element to its group's bounded
//! queue, suspending while that queue is full: one slow group backpressures the whole source.
//! Memory is at most `buffer` elements per open group, plus `buffer` announced groups not yet
//! pulled from the outer stream.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Mutex, Weak};

use super::pump::{Msg, PumpGuard, feed, guarded, pumped};
use super::stream::{DEFAULT_CHUNK_SIZE, Stream, lock, queue_source};
use crate::coordination::Queue;
use crate::failure::Never;
use crate::kernel::{Effect, succeed, suspend, sync};
use crate::runtime::FiberHandle;

/// The open groups' queues, shared by the router and the cleanup.
type Groups<K, A> = Arc<Mutex<HashMap<K, Queue<A>>>>;

/// Rides with a substream: dropping it (the substream ended or its consumer let go) shuts the
/// group's queue, so the router discards that key's later elements instead of blocking on them.
/// It also keeps the router running while the substream is alive.
struct GroupGuard<A: Send + 'static> {
  queue: Queue<A>,
  _pump: Arc<PumpGuard>,
}

impl<A: Send + 'static> Drop for GroupGuard<A> {
  fn drop(&mut self) {
    self.queue.shutdown_sync();
  }
}

/// The router's state, moved through its loop.
struct Router<K, A, E, R, F> {
  key: F,
  groups: Groups<K, A>,
  outer: Queue<Msg<(K, Stream<A, E, R>), E>>,
  /// Weak: the router lives on the pump fiber the guard owns.
  guard: Weak<PumpGuard>,
  buffer: usize,
}

impl<A, E, R> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Splits this stream by `key` into `(key, substream)` pairs, one per distinct key in order of
  /// first appearance (Effect.ts `Stream.groupByKey`). Each substream yields that key's elements
  /// in order and buffers at most `buffer` (at least 1) of them.
  ///
  /// The outer stream's first pull starts a router fiber (a child of the pulling fiber) that
  /// pulls upstream and routes each element. **Backpressure:** while a group's buffer is full
  /// the router waits, so the substreams must be consumed concurrently (fork each one, or use
  /// [`group_by`](Self::group_by)); a group that is never drained stalls every group. Dropping a
  /// substream discards its key's later elements. The router stops — its in-flight upstream
  /// pull interrupted, upstream dropped — once the outer stream and every substream are dropped.
  ///
  /// When upstream fails the outer stream fails with the error (after the groups announced
  /// before it); the substreams end.
  pub fn group_by_key<K, F>(self, buffer: usize, key: F) -> Stream<(K, Stream<A, E, R>), E, R>
  where
    K: Clone + Eq + Hash + Send + 'static,
    F: FnMut(&A) -> K + Send + 'static,
  {
    let buffer = buffer.max(1);
    let outer: Queue<Msg<(K, Stream<A, E, R>), E>> = Queue::bounded(buffer);
    let groups: Groups<K, A> = Arc::new(Mutex::new(HashMap::new()));
    let close = {
      let (outer, groups) = (outer.clone(), Arc::clone(&groups));
      move || {
        outer.shutdown_sync();
        for q in lock(&groups).values() {
          q.shutdown_sync();
        }
      }
    };
    let close_on_exit = close.clone();
    let guard = PumpGuard::new(close);
    let router = Router {
      key,
      groups,
      outer: outer.clone(),
      guard: Arc::downgrade(&guard),
      buffer,
    };
    let pump = route(self, router).ensuring(sync(close_on_exit));
    pumped(Some(pump), outer, guard, VecDeque::new())
  }

  /// Groups this stream by `key` and runs `f(key, substream)` for every group concurrently,
  /// merging their outputs in arrival order (Effect.ts `Stream.groupBy(…).evaluate`, or
  /// `groupByKey` + `flatMap` with unbounded concurrency).
  ///
  /// Each group buffers at most `buffer` (at least 1) input elements, and at most `buffer`
  /// output chunks wait for the consumer, so a slow consumer backpressures every group and the
  /// source. The first failure, of upstream or of a group's stream, fails the result. Dropping
  /// the result interrupts the group fibers and the router.
  ///
  /// ```
  /// use id_effect::{Exit, Stream};
  ///
  /// let sums = Stream::<u32>::from_iterable(1..=10)
  ///   .group_by(4, |n| n % 2, |parity, group| {
  ///     Stream::from_effect(group.run_fold(0, |acc, n| acc + n).map(move |s| vec![(parity, s)]))
  ///   });
  /// let mut out = match sums.run_collect().block_on() {
  ///   Exit::Success(v) => v,
  ///   Exit::Failure(c) => panic!("{c:?}"),
  /// };
  /// out.sort();
  /// assert_eq!(out, vec![(0, 30), (1, 25)]);
  /// ```
  pub fn group_by<K, B, F, G>(self, buffer: usize, key: F, mut f: G) -> Stream<B, E, R>
  where
    K: Clone + Eq + Hash + Send + 'static,
    B: Send + 'static,
    F: FnMut(&A) -> K + Send + 'static,
    G: FnMut(K, Stream<A, E, R>) -> Stream<B, E, R> + Send + 'static,
  {
    let buffer = buffer.max(1);
    let inners = self
      .group_by_key(buffer, key)
      .map_serial(move |(k, group)| f(k, group));
    let out: Queue<Msg<B, E>> = Queue::bounded(buffer);
    let close_on_drop = out.clone();
    let guard = PumpGuard::new(move || close_on_drop.shutdown_sync());
    let close_on_exit = out.clone();
    let pump = merge_all(inners, out.clone(), Vec::new())
      .ensuring(sync(move || close_on_exit.shutdown_sync()));
    pumped(Some(pump), out, guard, VecDeque::new())
  }
}

/// The router loop: pulls one upstream chunk, routes it, repeats; a failure is sent to the outer
/// stream.
fn route<K, A, E, R, F>(
  upstream: Stream<A, E, R>,
  router: Router<K, A, E, R, F>,
) -> Effect<(), Never, R>
where
  K: Clone + Eq + Hash + Send + 'static,
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  F: FnMut(&A) -> K + Send + 'static,
{
  suspend(move || upstream.pull_raw(DEFAULT_CHUNK_SIZE))
    .map(Ok)
    .catch(|e| succeed(Err(e)))
    .flat_map(move |step| match step {
      Ok(None) => succeed(()),
      Ok(Some((items, rest))) => {
        dispatch(items.into_iter(), router).flat_map(move |router| match router {
          Some(router) => route(rest, router),
          None => succeed(()),
        })
      }
      Err(e) => router.outer.offer(Err(e)).map(|_| ()),
    })
}

/// Offers each item to its group's queue (opening and announcing a group for a new key),
/// suspending while that queue is full. `None` when the router should stop (every consumer is
/// gone).
fn dispatch<K, A, E, R, F>(
  mut items: std::vec::IntoIter<A>,
  mut router: Router<K, A, E, R, F>,
) -> Effect<Option<Router<K, A, E, R, F>>, Never, R>
where
  K: Clone + Eq + Hash + Send + 'static,
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  F: FnMut(&A) -> K + Send + 'static,
{
  let Some(item) = items.next() else {
    return succeed(Some(router));
  };
  let k = (router.key)(&item);
  let existing = lock(&router.groups).get(&k).cloned();
  let (queue, announce) = match existing {
    Some(q) => (q, succeed(true)),
    None => {
      let Some(pump) = router.guard.upgrade() else {
        return succeed(None);
      };
      let q = Queue::bounded(router.buffer);
      let source = queue_source(
        q.clone(),
        Arc::new(Mutex::new(None)),
        VecDeque::new(),
        |v| v,
      );
      let group = guarded(
        source,
        GroupGuard {
          queue: q.clone(),
          _pump: pump,
        },
      );
      lock(&router.groups).insert(k.clone(), q.clone());
      // A refused announcement (the outer stream is gone) drops the substream, which shuts the
      // new queue: the key's elements are then discarded.
      (q, router.outer.offer(Ok(vec![(k, group)])))
    }
  };
  announce.flat_map(move |_| queue.offer(item).flat_map(move |_| dispatch(items, router)))
}

/// The merge loop of [`Stream::group_by`]: forks a feeder fiber per group stream (a child of
/// this pump, so interrupting the pump interrupts them), then awaits every feeder.
fn merge_all<B, E, R>(
  inners: Stream<Stream<B, E, R>, E, R>,
  out: Queue<Msg<B, E>>,
  mut feeders: Vec<FiberHandle<(), Never>>,
) -> Effect<(), Never, R>
where
  B: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  suspend(move || inners.pull_raw(1))
    .map(Ok)
    .catch(|e| succeed(Err(e)))
    .flat_map(move |step| match step {
      Ok(None) => join_all(feeders),
      Ok(Some((groups, rest))) => {
        let mut forks: Effect<Vec<FiberHandle<(), Never>>, Never, R> = succeed(Vec::new());
        for group in groups {
          let feeder = feed(group, out.clone(), DEFAULT_CHUNK_SIZE);
          forks = forks.flat_map(move |mut hs| {
            feeder.fork().map(move |h| {
              hs.push(h);
              hs
            })
          });
        }
        forks.flat_map(move |hs| {
          feeders.extend(hs);
          merge_all(rest, out, feeders)
        })
      }
      Err(e) => out.offer(Err(e)).map(|_| ()),
    })
}

/// Awaits every handle in order.
fn join_all<R: Send + Sync + 'static>(
  mut handles: Vec<FiberHandle<(), Never>>,
) -> Effect<(), Never, R> {
  match handles.pop() {
    None => succeed(()),
    Some(h) => h.join().flat_map(move |()| join_all(handles)),
  }
}
