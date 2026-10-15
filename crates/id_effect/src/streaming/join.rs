//! Two-sided stream combinators: [`Stream::merge`] (fair interleave), [`combine_latest`] and
//! [`keyed_join`].
//!
//! Each pull fetches at most one chunk from each side whose buffer is empty (left, then right;
//! the sides are pulled one after the other on the pulling fiber, not concurrently), so memory is
//! one chunk per side plus the documented operator state.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

use super::stream::Stream;
use crate::kernel::{Effect, succeed};

/// One input of a two-sided combinator: the rest of its stream (`None` once it ended) and its
/// pulled but not yet consumed items.
struct Side<A, E, R> {
  stream: Option<Stream<A, E, R>>,
  buf: VecDeque<A>,
}

impl<A, E, R> Side<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  fn new(stream: Stream<A, E, R>) -> Self {
    Self {
      stream: Some(stream),
      buf: VecDeque::new(),
    }
  }

  fn done(&self) -> bool {
    self.stream.is_none()
  }

  fn finished(&self) -> bool {
    self.done() && self.buf.is_empty()
  }

  /// Pulls one chunk into the buffer when it is empty and the side has not ended.
  fn refill(mut self, n: usize) -> Effect<Self, E, R> {
    if !self.buf.is_empty() {
      return succeed(self);
    }
    match self.stream.take() {
      None => succeed(self),
      Some(s) => s.pull_raw(n).map(move |step| {
        if let Some((items, rest)) = step {
          self.buf.extend(items);
          self.stream = Some(rest);
        }
        self
      }),
    }
  }
}

/// Refills both sides, then lets `emit` consume from their buffers; ends once both sides are
/// finished.
fn two_sided<A, B, C, S, F, E, R>(
  left: Side<A, E, R>,
  right: Side<B, E, R>,
  state: S,
  emit: F,
) -> Stream<C, E, R>
where
  A: Send + 'static,
  B: Send + 'static,
  C: Send + 'static,
  S: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  F: FnMut(&mut Side<A, E, R>, &mut Side<B, E, R>, &mut S) -> Vec<C> + Send + 'static,
{
  Stream::from_step(move |n| {
    left.refill(n).flat_map(move |left| {
      right.refill(n).map(move |right| {
        let (mut left, mut right, mut state, mut emit) = (left, right, state, emit);
        if left.finished() && right.finished() {
          return None;
        }
        let out = emit(&mut left, &mut right, &mut state);
        Some((out, two_sided(left, right, state, emit)))
      })
    })
  })
}

impl<A, E, R> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Fair merge: alternates elements of `self` and `other` until both end (the remainder of the
  /// longer side follows once the other has ended).
  #[must_use]
  pub fn merge(self, other: Self) -> Self {
    two_sided(
      Side::new(self),
      Side::new(other),
      true,
      |left, right, turn_left: &mut bool| {
        let mut out = Vec::new();
        loop {
          let (mine, theirs) = if *turn_left {
            (&mut *left, &mut *right)
          } else {
            (&mut *right, &mut *left)
          };
          if let Some(x) = mine.buf.pop_front() {
            out.push(x);
            *turn_left = !*turn_left;
          } else if mine.done() {
            match theirs.buf.pop_front() {
              Some(x) => out.push(x),
              None => break,
            }
          } else {
            break;
          }
        }
        out
      },
    )
  }
}

/// Pairs the latest value of each side whenever either side emits (once both have a value).
/// Retains only the latest value per side.
pub fn combine_latest<A, B, E, R>(
  left: Stream<A, E, R>,
  right: Stream<B, E, R>,
) -> Stream<(A, B), E, R>
where
  A: Clone + Send + 'static,
  B: Clone + Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  two_sided(
    Side::new(left),
    Side::new(right),
    (None::<A>, None::<B>),
    |left, right, (latest_a, latest_b)| {
      let mut out = Vec::new();
      for a in left.buf.drain(..) {
        if let Some(b) = latest_b.as_ref() {
          out.push((a.clone(), b.clone()));
        }
        *latest_a = Some(a);
      }
      for b in right.buf.drain(..) {
        if let Some(a) = latest_a.as_ref() {
          out.push((a.clone(), b.clone()));
        }
        *latest_b = Some(b);
      }
      out
    },
  )
}

/// Inner join on key: emits `(K, A, B)` when both sides have a value for the same `K`.
///
/// **Memory:** to match items that arrive at different times, the join keeps the latest value
/// of every distinct key seen on each side, so its state grows with the number of distinct keys
/// (not of items). Bound the key space (or window the inputs) for unbounded streams.
pub fn keyed_join<K, A, B, E, R>(
  left: Stream<(K, A), E, R>,
  right: Stream<(K, B), E, R>,
) -> Stream<(K, A, B), E, R>
where
  K: Clone + Hash + Eq + Send + 'static,
  A: Clone + Send + 'static,
  B: Clone + Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  two_sided(
    Side::new(left),
    Side::new(right),
    (HashMap::<K, A>::new(), HashMap::<K, B>::new()),
    |left, right, (left_map, right_map)| {
      let mut out = Vec::new();
      for (k, a) in left.buf.drain(..) {
        if let Some(b) = right_map.get(&k) {
          out.push((k.clone(), a.clone(), b.clone()));
        }
        left_map.insert(k, a);
      }
      for (k, b) in right.buf.drain(..) {
        if let Some(a) = left_map.get(&k) {
          out.push((k.clone(), a.clone(), b.clone()));
        }
        right_map.insert(k, b);
      }
      out
    },
  )
}
