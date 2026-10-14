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

use crate::streaming::stream::{StageStep, Stream};

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
    if size == 0 || step == 0 {
      return Stream::empty();
    }
    let mut buf: VecDeque<A> = VecDeque::new();
    self.stage(move |input| {
      let Some(vec) = input else {
        return StageStep::Emit(Vec::new());
      };
      let mut out: Vec<Vec<A>> = Vec::new();
      for item in vec {
        buf.push_back(item);
        while buf.len() >= size {
          out.push(buf.iter().take(size).cloned().collect());
          for _ in 0..step.min(buf.len()) {
            buf.pop_front();
          }
        }
      }
      StageStep::Emit(out)
    })
  }

  /// Session windows: emit a chunk when the gap between consecutive timestamps exceeds `gap`.
  ///
  /// Incremental: a session is emitted as soon as the first element of the next session arrives;
  /// the final session is flushed at end of stream. Buffers only the current session.
  #[inline]
  pub fn session_by_gap<F>(self, gap: Duration, mut timestamp: F) -> Stream<Vec<A>, E, R>
  where
    A: Clone,
    F: FnMut(&A) -> Instant + Send + 'static,
  {
    let mut cur: Vec<A> = Vec::new();
    let mut last_ts: Option<Instant> = None;
    self.stage(move |input| {
      let Some(vec) = input else {
        let last = core::mem::take(&mut cur);
        return StageStep::Emit(if last.is_empty() {
          Vec::new()
        } else {
          vec![last]
        });
      };
      let mut out: Vec<Vec<A>> = Vec::new();
      for item in vec {
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
      StageStep::Emit(out)
    })
  }

  /// Tumbling time windows: group elements whose timestamps fall in `[bucket, bucket + duration)`.
  ///
  /// Incremental: the watermark is the largest timestamp seen so far; a bucket is emitted once the
  /// watermark reaches its end (`bucket + duration`), and remaining buckets are flushed in order
  /// at end of stream. Out-of-order elements are placed in their bucket while it is still open; an
  /// element arriving after its bucket was emitted opens a fresh bucket for the same range. Memory
  /// is bounded by the elements of the currently open buckets.
  #[inline]
  pub fn tumbling_by_time<F>(self, duration: Duration, mut timestamp: F) -> Stream<Vec<A>, E, R>
  where
    A: Clone,
    F: FnMut(&A) -> Instant + Send + 'static,
  {
    let duration = duration.max(Duration::from_nanos(1));
    let mut buckets: Vec<(Instant, Vec<A>)> = Vec::new();
    let mut watermark: Option<Instant> = None;
    self.stage(move |input| {
      let Some(vec) = input else {
        buckets.sort_by_key(|(b, _)| *b);
        return StageStep::Emit(buckets.drain(..).map(|(_, v)| v).collect());
      };
      for item in vec {
        let ts = timestamp(&item);
        watermark = Some(watermark.map_or(ts, |w| w.max(ts)));
        let bucket_start = bucket_for(ts, duration);
        if let Some((_, v)) = buckets.iter_mut().find(|(b, _)| *b == bucket_start) {
          v.push(item);
        } else {
          buckets.push((bucket_start, vec![item]));
        }
      }
      let Some(wm) = watermark else {
        return StageStep::Emit(Vec::new());
      };
      buckets.sort_by_key(|(b, _)| *b);
      let closed = buckets
        .iter()
        .take_while(|(b, _)| *b + duration <= wm)
        .count();
      StageStep::Emit(buckets.drain(..closed).map(|(_, v)| v).collect())
    })
  }

  /// Sliding time windows: emit every `step`, each window covering `duration`.
  ///
  /// Incremental: window starts are aligned to `step`, beginning at the bucket of the earliest
  /// element seen before the first window is evaluated. A
  /// window `[start, start + duration)` is emitted (if non-empty) once the watermark (largest
  /// timestamp seen) reaches its end; elements older than the next window start are pruned, so
  /// memory is bounded by the elements within one `duration`. Remaining windows are flushed at end
  /// of stream. Elements older than an already-emitted window start are dropped.
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
    // Items sorted by timestamp, all at or after `next_start`.
    let mut items: VecDeque<(Instant, A)> = VecDeque::new();
    let mut next_start: Option<Instant> = None;
    let mut watermark: Option<Instant> = None;
    // Until the first window is evaluated, the first start follows the earliest timestamp seen
    // (as in the batch definition); afterwards it only moves forward.
    let mut advanced = false;
    self.stage(move |input| {
      let ended = input.is_none();
      for item in input.unwrap_or_default() {
        let ts = timestamp(&item);
        let bucket = bucket_for(ts, step);
        let start = match next_start {
          Some(s) if advanced || s <= bucket => s,
          _ => {
            next_start = Some(bucket);
            bucket
          }
        };
        watermark = Some(watermark.map_or(ts, |w| w.max(ts)));
        if ts < start {
          continue;
        }
        let pos = items.partition_point(|(t, _)| *t <= ts);
        items.insert(pos, (ts, item));
      }
      let (Some(mut start), Some(wm)) = (next_start, watermark) else {
        return StageStep::Emit(Vec::new());
      };
      // Open windows are complete once the watermark reaches their end; at end of stream, flush
      // every window that can still contain an element (same bound as the batch definition).
      let complete = |start: Instant| {
        if ended {
          start <= wm + duration
        } else {
          start + duration <= wm
        }
      };
      let mut out: Vec<Vec<A>> = Vec::new();
      while !items.is_empty() && complete(start) {
        let end = start + duration;
        let window: Vec<A> = items
          .iter()
          .take_while(|(ts, _)| *ts < end)
          .map(|(_, a)| a.clone())
          .collect();
        if !window.is_empty() {
          out.push(window);
        }
        start += step;
        advanced = true;
        while items.front().is_some_and(|(ts, _)| *ts < start) {
          items.pop_front();
        }
      }
      next_start = Some(start);
      StageStep::Emit(out)
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

    #[test]
    fn sliding_by_time_matches_batch_definition_for_ordered_input() {
      // Windows of 3s every 2s over t = 0, 2, 4: [0,3) [2,5) [4,7) (plus none empty).
      let stream = Stream::from_iterable([(ts(0), 1), (ts(2), 2), (ts(4), 3)]).sliding_by_time(
        Duration::from_secs(3),
        Duration::from_secs(2),
        |(t, _)| *t,
      );
      let out: Vec<Vec<i32>> = block_on(stream.run_collect().run(&mut ()))
        .expect("collect")
        .into_iter()
        .map(|w| w.into_iter().map(|(_, v)| v).collect())
        .collect();
      assert_eq!(out, vec![vec![1, 2], vec![2, 3], vec![3]]);
    }
  }

  mod sliding_by_time_order {
    use super::*;

    #[test]
    fn earlier_timestamp_arriving_later_before_first_window_is_kept() {
      let t = |ms: u64| window_epoch() + Duration::from_millis(ms);
      let stream = Stream::from_iterable([(t(10), 'a'), (t(0), 'b')]).sliding_by_time(
        Duration::from_millis(5),
        Duration::from_millis(5),
        |(ts, _)| *ts,
      );
      let out: Vec<Vec<char>> = block_on(stream.run_collect().run(&mut ()))
        .expect("collect")
        .into_iter()
        .map(|w| w.into_iter().map(|(_, c)| c).collect())
        .collect();
      assert_eq!(out, vec![vec!['b'], vec!['a']]);
    }

    #[test]
    fn earlier_timestamp_in_a_later_chunk_before_first_window_is_kept() {
      // Chunks of one item: the window [10,15) is not complete until a later watermark, so the
      // late `t0` item still re-anchors the first window start.
      let t = |ms: u64| window_epoch() + Duration::from_millis(ms);
      let mut stream = Stream::from_iterable([(t(10), 'a'), (t(0), 'b'), (t(30), 'c')])
        .sliding_by_time(
          Duration::from_millis(5),
          Duration::from_millis(5),
          |(ts, _)| *ts,
        );
      let mut out = Vec::new();
      while let Some(chunk) = block_on(stream.poll_next_chunk_with_size(&mut (), 1)).expect("poll")
      {
        out.extend(
          chunk
            .into_vec()
            .into_iter()
            .map(|w| w.into_iter().map(|(_, c)| c).collect::<Vec<_>>()),
        );
      }
      assert_eq!(out, vec![vec!['b'], vec!['a'], vec!['c']]);
    }
  }

  mod infinite_sources {
    use super::*;
    use crate::testing::assert_bounded_memory;

    /// Source item `n` → timestamp `n` seconds after the window epoch.
    fn secs(n: u64) -> Instant {
      instant_at(n)
    }

    #[test]
    fn tumbling_on_infinite_source_is_incremental() {
      let out = assert_bounded_memory(|s| s.tumbling(2), 3, 64);
      assert_eq!(out, vec![vec![0, 1], vec![2, 3], vec![4, 5]]);
    }

    #[test]
    fn sliding_on_infinite_source_is_incremental() {
      let out = assert_bounded_memory(|s| s.sliding(3, 2), 3, 64);
      assert_eq!(out, vec![vec![0, 1, 2], vec![2, 3, 4], vec![4, 5, 6]]);
    }

    #[test]
    fn session_by_gap_on_infinite_source_is_incremental() {
      // Timestamps 0,1,2 | 10,11,12 | 20,… : sessions of three separated by 8s gaps.
      let out = assert_bounded_memory(
        |s| s.session_by_gap(Duration::from_secs(5), |n| secs(n / 3 * 10 + n % 3)),
        2,
        64,
      );
      assert_eq!(out, vec![vec![0, 1, 2], vec![3, 4, 5]]);
    }

    #[test]
    fn tumbling_by_time_on_infinite_source_is_incremental() {
      let out = assert_bounded_memory(
        |s| s.tumbling_by_time(Duration::from_secs(4), |n| secs(*n)),
        2,
        64,
      );
      assert_eq!(out, vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7]]);
    }

    #[test]
    fn sliding_by_time_on_infinite_source_is_incremental() {
      let out = assert_bounded_memory(
        |s| s.sliding_by_time(Duration::from_secs(4), Duration::from_secs(2), |n| secs(*n)),
        3,
        64,
      );
      assert_eq!(
        out,
        vec![vec![0, 1, 2, 3], vec![2, 3, 4, 5], vec![4, 5, 6, 7]]
      );
    }
  }
}
