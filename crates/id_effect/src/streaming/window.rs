//! Window combinators on [`Stream`]: tumbling and sliding windows by count, session windows by
//! gap, and tumbling / sliding windows by time (from an element timestamp extractor).
//!
//! Every window operator is incremental: it pulls one upstream chunk per downstream pull and
//! buffers only its open windows.

use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::stream::{StageStep, Stream};

fn window_epoch() -> Instant {
  static EPOCH: OnceLock<Instant> = OnceLock::new();
  *EPOCH.get_or_init(Instant::now)
}

/// The start of the `width`-aligned bucket containing `ts`.
fn bucket_for(ts: Instant, width: Duration) -> Instant {
  let nanos = ts.saturating_duration_since(window_epoch()).as_nanos();
  let width_nanos = width.as_nanos().max(1);
  let bucket = nanos / width_nanos * width_nanos;
  window_epoch() + Duration::from_nanos(u64::try_from(bucket).unwrap_or(u64::MAX))
}

impl<A, E, R> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Non-overlapping windows of `size` elements (alias of [`grouped`](Self::grouped)).
  pub fn tumbling(self, size: usize) -> Stream<Vec<A>, E, R> {
    self.grouped(size)
  }

  /// Overlapping windows of `size` elements, advancing by `step` elements per window.
  /// `size == 0` or `step == 0` yields an empty stream.
  pub fn sliding(self, size: usize, step: usize) -> Stream<Vec<A>, E, R>
  where
    A: Clone,
  {
    if size == 0 || step == 0 {
      return Stream::empty();
    }
    let mut buf: VecDeque<A> = VecDeque::new();
    self.stage(move |input| {
      let Some(items) = input else {
        return StageStep::Emit(Vec::new());
      };
      let mut out: Vec<Vec<A>> = Vec::new();
      for item in items {
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

  /// Session windows: a window closes when the gap between consecutive timestamps exceeds
  /// `gap`. A session is emitted as soon as the first element of the next one arrives; the last
  /// session is flushed at the end.
  pub fn session_by_gap<F>(self, gap: Duration, mut timestamp: F) -> Stream<Vec<A>, E, R>
  where
    F: FnMut(&A) -> Instant + Send + 'static,
  {
    let mut cur: Vec<A> = Vec::new();
    let mut last_ts: Option<Instant> = None;
    self.stage(move |input| {
      let Some(items) = input else {
        let last = std::mem::take(&mut cur);
        return StageStep::Emit(if last.is_empty() {
          Vec::new()
        } else {
          vec![last]
        });
      };
      let mut out: Vec<Vec<A>> = Vec::new();
      for item in items {
        let ts = timestamp(&item);
        if let Some(prev) = last_ts
          && ts.saturating_duration_since(prev) > gap
          && !cur.is_empty()
        {
          out.push(std::mem::take(&mut cur));
        }
        cur.push(item);
        last_ts = Some(ts);
      }
      StageStep::Emit(out)
    })
  }

  /// Tumbling time windows: groups elements whose timestamps fall in `[bucket, bucket +
  /// duration)`. The watermark is the largest timestamp seen; a bucket is emitted once the
  /// watermark reaches its end, and the open buckets are flushed in order at the end. An element
  /// arriving after its bucket was emitted opens a fresh bucket for the same range.
  pub fn tumbling_by_time<F>(self, duration: Duration, mut timestamp: F) -> Stream<Vec<A>, E, R>
  where
    F: FnMut(&A) -> Instant + Send + 'static,
  {
    let duration = duration.max(Duration::from_nanos(1));
    let mut buckets: Vec<(Instant, Vec<A>)> = Vec::new();
    let mut watermark: Option<Instant> = None;
    self.stage(move |input| {
      let Some(items) = input else {
        buckets.sort_by_key(|(b, _)| *b);
        return StageStep::Emit(buckets.drain(..).map(|(_, v)| v).collect());
      };
      for item in items {
        let ts = timestamp(&item);
        watermark = Some(watermark.map_or(ts, |w| w.max(ts)));
        let start = bucket_for(ts, duration);
        if let Some((_, v)) = buckets.iter_mut().find(|(b, _)| *b == start) {
          v.push(item);
        } else {
          buckets.push((start, vec![item]));
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

  /// Sliding time windows of `duration`, one every `step`. Window starts are aligned to `step`,
  /// beginning at the bucket of the earliest element seen before the first window is evaluated.
  /// A window is emitted (if non-empty) once the watermark reaches its end; elements older than
  /// the next window start are pruned, so memory is bounded by one `duration` of elements. The
  /// remaining windows are flushed at the end; elements older than an emitted window start are
  /// dropped.
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
    // Until the first window is evaluated, the first start follows the earliest timestamp seen;
    // afterwards it only moves forward.
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

/// The instant `offset` after the epoch that time windows align their buckets to (a test
/// helper: timestamps built from it fall on predictable bucket boundaries).
#[doc(hidden)]
pub fn window_instant(offset: Duration) -> Instant {
  window_epoch() + offset
}
