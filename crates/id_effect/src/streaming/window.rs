//! Window combinators — tumbling, sliding, and session grouping on [`Stream`].
//!
//! Count-based windows partition elements by cardinality; time-based windows use an
//! element timestamp extractor. Session windows close when the gap between consecutive
//! timestamps exceeds `gap`.

use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn window_epoch() -> Instant {
  static EPOCH: OnceLock<Instant> = OnceLock::new();
  *EPOCH.get_or_init(Instant::now)
}

use crate::streaming::stream::Stream;

impl<A, E, R> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  /// Non-overlapping chunks of `size` elements (alias of [`Stream::grouped`]).
  #[inline]
  pub fn tumbling(self, size: usize) -> Stream<Vec<A>, E, R> {
    self.grouped(size)
  }

  /// Overlapping windows of length `size`, advancing by `step` elements per emission.
  ///
  /// `size == 0` or `step == 0` yields an empty stream.
  #[inline]
  pub fn sliding(self, size: usize, step: usize) -> Stream<Vec<A>, E, R>
  where
    A: Clone,
  {
    Stream::new(move |r: &mut R| {
      Box::pin(async move {
        if size == 0 || step == 0 {
          return Ok(Vec::new());
        }
        let mut upstream = self;
        let mut buf: VecDeque<A> = VecDeque::new();
        let mut out: Vec<Vec<A>> = Vec::new();
        loop {
          let Some(chunk) = upstream.poll_next_chunk(r).await? else {
            break;
          };
          for item in chunk.into_vec() {
            buf.push_back(item);
            while buf.len() >= size {
              let window: Vec<A> = buf.iter().take(size).cloned().collect();
              out.push(window);
              for _ in 0..step.min(buf.len()) {
                buf.pop_front();
              }
            }
          }
        }
        Ok(out)
      })
    })
  }

  /// Session windows: emit a chunk when the gap between consecutive timestamps exceeds `gap`.
  #[inline]
  pub fn session_by_gap<F>(self, gap: Duration, mut timestamp: F) -> Stream<Vec<A>, E, R>
  where
    A: Clone,
    F: FnMut(&A) -> Instant + Send + 'static,
  {
    Stream::new(move |r: &mut R| {
      Box::pin(async move {
        let mut upstream = self;
        let mut out: Vec<Vec<A>> = Vec::new();
        let mut cur: Vec<A> = Vec::new();
        let mut last_ts: Option<Instant> = None;
        loop {
          let Some(chunk) = upstream.poll_next_chunk(r).await? else {
            break;
          };
          for item in chunk.into_vec() {
            let ts = timestamp(&item);
            if let Some(prev) = last_ts
              && ts.saturating_duration_since(prev) > gap
              && !cur.is_empty()
            {
              out.push(core::mem::take(&mut cur));
            }
            cur.push(item);
            last_ts = Some(ts);
          }
        }
        if !cur.is_empty() {
          out.push(cur);
        }
        Ok(out)
      })
    })
  }

  /// Tumbling time windows: group elements whose timestamps fall in `[bucket, bucket + duration)`.
  #[inline]
  pub fn tumbling_by_time<F>(self, duration: Duration, mut timestamp: F) -> Stream<Vec<A>, E, R>
  where
    A: Clone,
    F: FnMut(&A) -> Instant + Send + 'static,
  {
    let duration = duration.max(Duration::from_nanos(1));
    Stream::new(move |r: &mut R| {
      Box::pin(async move {
        let mut upstream = self;
        let mut buckets: Vec<(Instant, Vec<A>)> = Vec::new();
        loop {
          let Some(chunk) = upstream.poll_next_chunk(r).await? else {
            break;
          };
          for item in chunk.into_vec() {
            let ts = timestamp(&item);
            let bucket_start = bucket_for(ts, duration);
            if let Some((_, vec)) = buckets.iter_mut().find(|(b, _)| *b == bucket_start) {
              vec.push(item);
            } else {
              buckets.push((bucket_start, vec![item]));
            }
          }
        }
        buckets.sort_by_key(|(b, _)| *b);
        Ok(buckets.into_iter().map(|(_, v)| v).collect())
      })
    })
  }

  /// Sliding time windows: emit every `step`, each window covering `duration`.
  #[inline]
  pub fn sliding_by_time<F>(
    self,
    duration: Duration,
    step: Duration,
    mut timestamp: F,
  ) -> Stream<Vec<A>, E, R>
  where
    A: Clone,
    F: FnMut(&A) -> Instant + Send + 'static,
  {
    let duration = duration.max(Duration::from_nanos(1));
    let step = step.max(Duration::from_nanos(1));
    Stream::new(move |r: &mut R| {
      Box::pin(async move {
        let mut upstream = self;
        let mut items: Vec<(Instant, A)> = Vec::new();
        loop {
          let Some(chunk) = upstream.poll_next_chunk(r).await? else {
            break;
          };
          for item in chunk.into_vec() {
            items.push((timestamp(&item), item));
          }
        }
        if items.is_empty() {
          return Ok(Vec::new());
        }
        items.sort_by_key(|(ts, _)| *ts);
        let min_ts = items[0].0;
        let max_ts = items.last().expect("non-empty").0;
        let mut out: Vec<Vec<A>> = Vec::new();
        let mut window_start = bucket_for(min_ts, step);
        let end = max_ts + duration;
        while window_start <= end {
          let window_end = window_start + duration;
          let window: Vec<A> = items
            .iter()
            .filter(|(ts, _)| *ts >= window_start && *ts < window_end)
            .map(|(_, a)| a.clone())
            .collect();
          if !window.is_empty() {
            out.push(window);
          }
          window_start += step;
        }
        Ok(out)
      })
    })
  }
}

#[inline]
fn bucket_for(ts: Instant, width: Duration) -> Instant {
  let nanos = ts.duration_since(window_epoch()).as_nanos();
  let width_nanos = width.as_nanos().max(1);
  let bucket = nanos / width_nanos * width_nanos;
  window_epoch() + Duration::from_nanos(bucket as u64)
}

#[cfg(test)]
mod tests {
  fn instant_at(secs: u64) -> Instant {
    window_epoch() + Duration::from_secs(secs)
  }

  use super::*;
  use std::time::Duration;

  fn block_on<F: core::future::Future>(fut: F) -> F::Output {
    pollster::block_on(fut)
  }

  mod tumbling {
    use super::*;

    #[test]
    fn groups_non_overlapping_chunks() {
      let stream = Stream::from_iterable([1, 2, 3, 4, 5]).tumbling(2);
      let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
      assert_eq!(out, vec![vec![1, 2], vec![3, 4], vec![5]]);
    }
  }

  mod sliding {
    use super::*;

    #[test]
    fn emits_overlapping_windows() {
      let stream = Stream::from_iterable([1, 2, 3, 4]).sliding(3, 1);
      let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
      assert_eq!(out, vec![vec![1, 2, 3], vec![2, 3, 4]]);
    }

    #[test]
    fn zero_size_yields_empty() {
      let stream = Stream::from_iterable([1, 2]).sliding(0, 1);
      let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
      assert!(out.is_empty());
    }
  }

  mod session {
    use super::*;

    fn ts(secs: u64) -> Instant {
      instant_at(secs)
    }

    #[test]
    fn splits_on_gap() {
      let items = vec![(ts(0), 1), (ts(1), 2), (ts(10), 3)];
      let stream = Stream::from_iterable(items).session_by_gap(Duration::from_secs(5), |(t, _)| *t);
      let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
      assert_eq!(out, vec![vec![(ts(0), 1), (ts(1), 2)], vec![(ts(10), 3)]]);
    }
  }

  mod time_windows {
    use super::*;

    fn ts(secs: u64) -> Instant {
      instant_at(secs)
    }

    #[test]
    fn tumbling_by_time_groups_into_buckets() {
      let stream = Stream::from_iterable([(ts(0), 'a'), (ts(1), 'b'), (ts(10), 'c')])
        .tumbling_by_time(Duration::from_secs(5), |(t, _)| *t);
      let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
      assert_eq!(out.len(), 2);
      assert_eq!(out[0], vec![(ts(0), 'a'), (ts(1), 'b')]);
      assert_eq!(out[1], vec![(ts(10), 'c')]);
    }

    #[test]
    fn sliding_by_time_emits_overlapping_ranges() {
      let stream = Stream::from_iterable([(ts(0), 1), (ts(2), 2), (ts(4), 3)]).sliding_by_time(
        Duration::from_secs(3),
        Duration::from_secs(2),
        |(t, _)| *t,
      );
      let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
      assert!(!out.is_empty());
      assert!(out.iter().all(|w| !w.is_empty()));
    }
  }
}
