//! Stream joins — merge, combine-latest, and keyed inner join.
//!
//! [`Stream::merge`] fairly interleaves two streams at the element level.
//! [`combine_latest`] pairs the latest value from each side whenever either updates.
//! [`keyed_join`] emits `(K, A, B)` when both sides have a value for the same key.

use std::collections::HashMap;

use crate::streaming::stream::Stream;

impl<A, E, R> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  /// Fair merge: alternates elements from `self` and `other` until both exhaust.
  #[inline]
  pub fn merge(self, other: Self) -> Self {
    Stream::new(move |r: &mut R| {
      Box::pin(async move {
        let mut left = self;
        let mut right = other;
        let mut out = Vec::new();
        let mut left_buf = Vec::new();
        let mut right_buf = Vec::new();
        let mut left_done = false;
        let mut right_done = false;
        let mut turn_left = true;

        loop {
          if left_buf.is_empty() && !left_done {
            match left.poll_next_chunk(r).await? {
              Some(chunk) => left_buf.extend(chunk.into_vec()),
              None => left_done = true,
            }
          }
          if right_buf.is_empty() && !right_done {
            match right.poll_next_chunk(r).await? {
              Some(chunk) => right_buf.extend(chunk.into_vec()),
              None => right_done = true,
            }
          }

          if left_buf.is_empty() && right_buf.is_empty() && left_done && right_done {
            break;
          }

          if turn_left {
            if !left_buf.is_empty() {
              out.push(left_buf.remove(0));
            } else if left_done {
              out.append(&mut right_buf);
              break;
            } else {
              turn_left = false;
              continue;
            }
          } else if !right_buf.is_empty() {
            out.push(right_buf.remove(0));
          } else if right_done {
            out.append(&mut left_buf);
            break;
          }

          turn_left = !turn_left;
        }
        Ok(out)
      })
    })
  }
}

/// Pair the latest value from each stream whenever either side emits.
#[inline]
pub fn combine_latest<A, B, E, R>(
  mut left: Stream<A, E, R>,
  mut right: Stream<B, E, R>,
) -> Stream<(A, B), E, R>
where
  A: Send + Clone + 'static,
  B: Send + Clone + 'static,
  E: Send + 'static,
  R: 'static,
{
  Stream::new(move |r: &mut R| {
    Box::pin(async move {
      let mut latest_a: Option<A> = None;
      let mut latest_b: Option<B> = None;
      let mut out: Vec<(A, B)> = Vec::new();
      let mut left_buf = Vec::new();
      let mut right_buf = Vec::new();
      let mut left_done = false;
      let mut right_done = false;

      loop {
        if left_buf.is_empty() && !left_done {
          match left.poll_next_chunk(r).await? {
            Some(chunk) => left_buf.extend(chunk.into_vec()),
            None => left_done = true,
          }
        }
        if right_buf.is_empty() && !right_done {
          match right.poll_next_chunk(r).await? {
            Some(chunk) => right_buf.extend(chunk.into_vec()),
            None => right_done = true,
          }
        }

        if left_buf.is_empty() && right_buf.is_empty() && left_done && right_done {
          break;
        }

        if !left_buf.is_empty() {
          let a = left_buf.remove(0);
          latest_a = Some(a.clone());
          if let Some(ref b) = latest_b {
            out.push((a, b.clone()));
          }
        } else if !right_buf.is_empty() {
          let b = right_buf.remove(0);
          latest_b = Some(b.clone());
          if let Some(ref a) = latest_a {
            out.push((a.clone(), b));
          }
        }
      }
      Ok(out)
    })
  })
}

/// Inner join on key: emits `(K, A, B)` when both streams have a value for the same `K`.
#[inline]
pub fn keyed_join<K, A, B, E, R>(
  mut left: Stream<(K, A), E, R>,
  mut right: Stream<(K, B), E, R>,
) -> Stream<(K, A, B), E, R>
where
  K: Send + Clone + std::hash::Hash + Eq + 'static,
  A: Send + Clone + 'static,
  B: Send + Clone + 'static,
  E: Send + 'static,
  R: 'static,
{
  Stream::new(move |r: &mut R| {
    Box::pin(async move {
      let mut left_map: HashMap<K, A> = HashMap::new();
      let mut right_map: HashMap<K, B> = HashMap::new();
      let mut out: Vec<(K, A, B)> = Vec::new();
      let mut left_done = false;
      let mut right_done = false;

      while !(left_done && right_done) {
        if !left_done {
          match left.poll_next_chunk(r).await? {
            Some(chunk) => {
              for (k, a) in chunk.into_vec() {
                if let Some(b) = right_map.get(&k) {
                  out.push((k.clone(), a.clone(), b.clone()));
                }
                left_map.insert(k, a);
              }
            }
            None => left_done = true,
          }
        }
        if !right_done {
          match right.poll_next_chunk(r).await? {
            Some(chunk) => {
              for (k, b) in chunk.into_vec() {
                if let Some(a) = left_map.get(&k) {
                  out.push((k.clone(), a.clone(), b.clone()));
                }
                right_map.insert(k, b);
              }
            }
            None => right_done = true,
          }
        }
      }
      Ok(out)
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
}
