//! Stream joins — merge, combine-latest, and keyed inner join.
//!
//! [`Stream::merge`] fairly interleaves two streams at the element level.
//! [`combine_latest`] pairs the latest value from each side whenever either updates.
//! [`keyed_join`] emits `(K, A, B)` when both sides have a value for the same key.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::streaming::stream::Stream;

impl<A, E, R> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  /// Fair merge: alternates elements from `self` and `other` until both exhaust.
  ///
  /// Incremental: each pull fetches at most one chunk from each side (only for a side whose buffer
  /// is empty) and interleaves elements, so memory is bounded by one chunk per side. Sides are
  /// pulled sequentially, not concurrently.
  #[inline]
  pub fn merge(self, other: Self) -> Self {
    struct Merge<A, E, R>
    where
      A: Send + 'static,
      E: Send + 'static,
      R: 'static,
    {
      left: Side<A, E, R>,
      right: Side<A, E, R>,
      turn_left: bool,
    }
    let shared = Arc::new(Mutex::new(Merge {
      left: Side::new(self),
      right: Side::new(other),
      turn_left: true,
    }));
    Stream::from_pull(move |r: &mut R, n| {
      let shared = Arc::clone(&shared);
      Box::pin(async move {
        refill(&shared, |m| &mut m.left, &mut *r, n).await?;
        refill(&shared, |m| &mut m.right, &mut *r, n).await?;
        let mut g = shared.lock().expect("merge mutex poisoned");
        let m = &mut *g;
        if m.left.finished() && m.right.finished() {
          return Ok(None);
        }
        let mut out = Vec::new();
        loop {
          let (mine, theirs) = if m.turn_left {
            (&mut m.left, &mut m.right)
          } else {
            (&mut m.right, &mut m.left)
          };
          if let Some(x) = mine.buf.pop_front() {
            out.push(x);
            m.turn_left = !m.turn_left;
          } else if mine.done {
            match theirs.buf.pop_front() {
              Some(x) => out.push(x),
              None => break,
            }
          } else {
            break;
          }
        }
        Ok(Some(out))
      })
    })
  }
}

/// One input of a two-sided join: upstream, its not-yet-consumed items, and whether it ended.
struct Side<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  stream: Stream<A, E, R>,
  buf: VecDeque<A>,
  done: bool,
}

impl<A, E, R> Side<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  fn new(stream: Stream<A, E, R>) -> Self {
    Self {
      stream,
      buf: VecDeque::new(),
      done: false,
    }
  }

  fn finished(&self) -> bool {
    self.done && self.buf.is_empty()
  }
}

/// Pull one chunk into the side selected by `side` if its buffer is empty and it has not ended.
/// The lock is not held across the upstream await.
async fn refill<T, X, E, R>(
  shared: &Mutex<T>,
  side: fn(&mut T) -> &mut Side<X, E, R>,
  r: &mut R,
  n: usize,
) -> Result<(), E>
where
  X: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  let next = {
    let mut g = shared.lock().expect("join mutex poisoned");
    let s = side(&mut g);
    if s.done || !s.buf.is_empty() {
      return Ok(());
    }
    s.stream.poll_next_chunk_with_size(r, n)
  };
  let next = next.await?;
  let mut g = shared.lock().expect("join mutex poisoned");
  let s = side(&mut g);
  match next {
    Some(chunk) => s.buf.extend(chunk.into_vec()),
    None => s.done = true,
  }
  Ok(())
}

/// Pair the latest value from each stream whenever either side emits.
///
/// Incremental: each pull fetches at most one chunk from each side, then applies the left chunk
/// followed by the right chunk, emitting a pair for every new item once both sides have produced
/// a value. Retains only the latest value per side.
#[inline]
pub fn combine_latest<A, B, E, R>(
  left: Stream<A, E, R>,
  right: Stream<B, E, R>,
) -> Stream<(A, B), E, R>
where
  A: Send + Clone + 'static,
  B: Send + Clone + 'static,
  E: Send + 'static,
  R: 'static,
{
  struct Latest<A, B, E, R>
  where
    A: Send + 'static,
    B: Send + 'static,
    E: Send + 'static,
    R: 'static,
  {
    left: Side<A, E, R>,
    right: Side<B, E, R>,
    latest_a: Option<A>,
    latest_b: Option<B>,
  }
  let shared = Arc::new(Mutex::new(Latest {
    left: Side::new(left),
    right: Side::new(right),
    latest_a: None,
    latest_b: None,
  }));
  Stream::from_pull(move |r: &mut R, n| {
    let shared = Arc::clone(&shared);
    Box::pin(async move {
      refill(&shared, |s| &mut s.left, &mut *r, n).await?;
      refill(&shared, |s| &mut s.right, &mut *r, n).await?;
      let mut g = shared.lock().expect("combine_latest mutex poisoned");
      let s = &mut *g;
      if s.left.finished() && s.right.finished() {
        return Ok(None);
      }
      let mut out: Vec<(A, B)> = Vec::new();
      for a in s.left.buf.drain(..) {
        if let Some(b) = &s.latest_b {
          out.push((a.clone(), b.clone()));
        }
        s.latest_a = Some(a);
      }
      for b in s.right.buf.drain(..) {
        if let Some(a) = &s.latest_a {
          out.push((a.clone(), b.clone()));
        }
        s.latest_b = Some(b);
      }
      Ok(Some(out))
    })
  })
}

/// Inner join on key: emits `(K, A, B)` when both streams have a value for the same `K`.
///
/// Incremental: each pull fetches at most one chunk from each side and emits matches against the
/// values seen so far. **Memory:** to match items that arrive at different times, the join
/// retains the latest value for every distinct key seen on each side, so state grows with the
/// number of distinct keys (not with the number of items). Bound the key space (or window the
/// inputs) for unbounded streams.
#[inline]
pub fn keyed_join<K, A, B, E, R>(
  left: Stream<(K, A), E, R>,
  right: Stream<(K, B), E, R>,
) -> Stream<(K, A, B), E, R>
where
  K: Send + Clone + std::hash::Hash + Eq + 'static,
  A: Send + Clone + 'static,
  B: Send + Clone + 'static,
  E: Send + 'static,
  R: 'static,
{
  struct Join<K, A, B, E, R>
  where
    K: Send + 'static,
    A: Send + 'static,
    B: Send + 'static,
    E: Send + 'static,
    R: 'static,
  {
    left: Side<(K, A), E, R>,
    right: Side<(K, B), E, R>,
    left_map: HashMap<K, A>,
    right_map: HashMap<K, B>,
  }
  let shared = Arc::new(Mutex::new(Join {
    left: Side::new(left),
    right: Side::new(right),
    left_map: HashMap::new(),
    right_map: HashMap::new(),
  }));
  Stream::from_pull(move |r: &mut R, n| {
    let shared = Arc::clone(&shared);
    Box::pin(async move {
      refill(&shared, |j| &mut j.left, &mut *r, n).await?;
      refill(&shared, |j| &mut j.right, &mut *r, n).await?;
      let mut g = shared.lock().expect("keyed_join mutex poisoned");
      let j = &mut *g;
      if j.left.finished() && j.right.finished() {
        return Ok(None);
      }
      let mut out: Vec<(K, A, B)> = Vec::new();
      for (k, a) in j.left.buf.drain(..) {
        if let Some(b) = j.right_map.get(&k) {
          out.push((k.clone(), a.clone(), b.clone()));
        }
        j.left_map.insert(k, a);
      }
      for (k, b) in j.right.buf.drain(..) {
        if let Some(a) = j.left_map.get(&k) {
          out.push((k.clone(), a.clone(), b.clone()));
        }
        j.right_map.insert(k, b);
      }
      Ok(Some(out))
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn block_on<F: core::future::Future>(fut: F) -> F::Output {
    pollster::block_on(fut)
  }

  mod merge {
    use super::*;

    #[test]
    fn interleaves_two_streams() {
      let a = Stream::from_iterable([1, 2]);
      let b = Stream::from_iterable([10, 20]);
      let out = block_on(a.merge(b).run_collect().run(&mut ())).expect("collect");
      assert_eq!(out, vec![1, 10, 2, 20]);
    }

    #[test]
    fn drains_remaining_when_one_side_finishes_first() {
      let a = Stream::from_iterable([1]);
      let b = Stream::from_iterable([10, 20, 30]);
      let out = block_on(a.merge(b).run_collect().run(&mut ())).expect("collect");
      assert_eq!(out, vec![1, 10, 20, 30]);
    }
  }

  mod combine_latest {
    use super::*;

    #[test]
    fn pairs_latest_on_each_update() {
      let left = Stream::from_iterable([1, 2]);
      let right = Stream::from_iterable(['a', 'b']);
      let out = block_on(combine_latest(left, right).run_collect().run(&mut ())).expect("collect");
      assert_eq!(out, vec![(2, 'a'), (2, 'b')]);
    }
  }

  mod keyed_join {
    use super::*;

    #[test]
    fn joins_matching_keys() {
      let left = Stream::from_iterable([("x", 1), ("y", 2)]);
      let right = Stream::from_iterable([("x", 'a'), ("z", 'b')]);
      let out = block_on(keyed_join(left, right).run_collect().run(&mut ())).expect("collect");
      assert_eq!(out, vec![("x", 1, 'a')]);
    }
  }

  mod infinite_sources {
    use super::*;
    use crate::testing::assert_bounded_memory;

    #[test]
    fn merge_with_long_finite_side_keeps_every_element() {
      let a = Stream::from_iterable([1]);
      let b = Stream::from_iterable(100..300);
      let out = block_on(a.merge(b).run_collect().run(&mut ())).expect("collect");
      let mut expected = vec![1];
      expected.extend(100..300);
      assert_eq!(out, expected);
    }

    #[test]
    fn merge_with_infinite_sides_is_incremental_and_fair() {
      let out = assert_bounded_memory(|s| s.merge(Stream::from_iterable(1000_u64..)), 6, 64);
      assert_eq!(out, vec![0, 1000, 1, 1001, 2, 1002]);
    }

    #[test]
    fn combine_latest_with_infinite_sides_is_incremental() {
      let out = assert_bounded_memory(
        |s| combine_latest(s, Stream::from_iterable(['a', 'b'].into_iter().cycle())),
        3,
        64,
      );
      // Under `take(3)` each side yields a 3-item chunk: left 0,1,2 (no right value yet), then
      // right 'a','b','a' each pair with the latest left value 2.
      assert_eq!(out, vec![(2, 'a'), (2, 'b'), (2, 'a')]);
    }

    #[test]
    fn keyed_join_with_infinite_sides_is_incremental() {
      let out = assert_bounded_memory(
        |s| {
          keyed_join(
            s.map(|n| (n % 4, n)),
            Stream::from_iterable((0_u64..).map(|n| (n % 4, n * 10))),
          )
        },
        4,
        64,
      );
      assert_eq!(out, vec![(0, 0, 0), (1, 1, 10), (2, 2, 20), (3, 3, 30)]);
    }
  }
}
