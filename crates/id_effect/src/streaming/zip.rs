//! Positional pairing of two streams: [`Stream::zip`], [`Stream::zip_with`], and the method form
//! of [`combine_latest`](super::combine_latest).
//!
//! Each pull fetches at most one chunk from each side whose buffer is empty (left, then right, on
//! the pulling fiber), so memory is one chunk per side. The zip ends as soon as either side ends:
//! the other side is dropped then, without being pulled again.

use std::collections::VecDeque;

use super::stream::Stream;
use crate::kernel::{Effect, succeed};

impl<A, E, R> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Pairs the elements of `self` and `other` by position (Effect.ts `Stream.zip`); ends with the
  /// shorter side.
  ///
  /// ```
  /// use id_effect::{Exit, Stream};
  ///
  /// let pairs: Stream<(u32, char)> =
  ///   Stream::from_iterable(1..).zip(Stream::from_iterable(['a', 'b', 'c']));
  /// assert_eq!(
  ///   pairs.run_collect().block_on(),
  ///   Exit::succeed(vec![(1, 'a'), (2, 'b'), (3, 'c')])
  /// );
  /// ```
  pub fn zip<B>(self, other: Stream<B, E, R>) -> Stream<(A, B), E, R>
  where
    B: Send + 'static,
  {
    self.zip_with(other, |a, b| (a, b))
  }

  /// Combines the elements of `self` and `other` by position with `f` (Effect.ts
  /// `Stream.zipWith`); ends with the shorter side.
  pub fn zip_with<B, C, F>(self, other: Stream<B, E, R>, f: F) -> Stream<C, E, R>
  where
    B: Send + 'static,
    C: Send + 'static,
    F: FnMut(A, B) -> C + Send + 'static,
  {
    zip_go(Some(self), VecDeque::new(), Some(other), VecDeque::new(), f)
  }

  /// Pairs the latest element of each side whenever either side emits, once both have emitted
  /// (Effect.ts `Stream.zipLatest`); the method form of [`combine_latest`](super::combine_latest).
  /// Retains only the latest element per side.
  pub fn combine_latest<B>(self, other: Stream<B, E, R>) -> Stream<(A, B), E, R>
  where
    A: Clone,
    B: Clone + Send + 'static,
  {
    super::join::combine_latest(self, other)
  }
}

/// Pulls one chunk of `stream` into `buf` when `buf` is empty; `None` once the stream ended.
fn refill<A, E, R>(
  stream: Option<Stream<A, E, R>>,
  buf: VecDeque<A>,
  n: usize,
) -> Effect<(Option<Stream<A, E, R>>, VecDeque<A>), E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  match stream {
    Some(s) if buf.is_empty() => s.pull_raw(n).map(move |step| match step {
      None => (None, buf),
      Some((items, rest)) => (Some(rest), items.into()),
    }),
    stream => succeed((stream, buf)),
  }
}

/// The state machine of [`Stream::zip_with`]: each side's rest and its pulled, unpaired items.
fn zip_go<A, B, C, F, E, R>(
  left: Option<Stream<A, E, R>>,
  lbuf: VecDeque<A>,
  right: Option<Stream<B, E, R>>,
  rbuf: VecDeque<B>,
  f: F,
) -> Stream<C, E, R>
where
  A: Send + 'static,
  B: Send + 'static,
  C: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  F: FnMut(A, B) -> C + Send + 'static,
{
  Stream::from_step(move |n| {
    refill(left, lbuf, n).flat_map(move |(left, lbuf)| {
      if lbuf.is_empty() {
        // Ended (the right side is dropped unpulled), or an empty chunk: pull again.
        return succeed(left.map(|l| (Vec::new(), zip_go(Some(l), lbuf, right, rbuf, f))));
      }
      refill(right, rbuf, n).map(move |(right, rbuf)| {
        let (mut lbuf, mut rbuf, mut f) = (lbuf, rbuf, f);
        if rbuf.is_empty() {
          return right.map(|r| (Vec::new(), zip_go(left, lbuf, Some(r), rbuf, f)));
        }
        let k = lbuf.len().min(rbuf.len());
        let out: Vec<C> = lbuf
          .drain(..k)
          .zip(rbuf.drain(..k))
          .map(|(a, b)| f(a, b))
          .collect();
        Some((out, zip_go(left, lbuf, right, rbuf, f)))
      })
    })
  })
}
