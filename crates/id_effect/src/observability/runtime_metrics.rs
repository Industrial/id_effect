//! Runtime metrics: process-wide fiber counters kept by the kernel, read at scrape time.
//!
//! Every [`FiberContext`](crate::FiberContext) counts itself when it is built (roots, children
//! and detached fibers alike) and uncounts itself once, when it is marked done or, for a context
//! that never completes through a run edge, when it is dropped. The counters are two relaxed
//! process-wide atomics: one `fetch_add` pair per fiber start and one `fetch_sub` per fiber end,
//! no registry lookup and no lock on the fork path.
//!
//! They are not registry metrics (nothing records into a [`MetricRegistry`] per fork):
//! [`snapshot`] reads them as [`MetricPair`]s for an exporter to append to a registry snapshot,
//! as the Prometheus endpoint in `id_effect_axum` does.
//!
//! [`MetricRegistry`]: super::MetricRegistry

use std::sync::atomic::{AtomicU64, Ordering};

use super::metric::{MetricKey, MetricKind, MetricState};
use super::registry::MetricPair;

/// Gauge name: fibers built and not yet done (or dropped).
pub const FIBERS_LIVE: &str = "id_effect_fibers_live";

/// Counter name: fibers built since the process started.
pub const FIBERS_STARTED_TOTAL: &str = "id_effect_fibers_started_total";

static STARTED: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);

/// A fiber context was built.
#[inline]
pub(crate) fn fiber_started() {
  STARTED.fetch_add(1, Ordering::Relaxed);
  LIVE.fetch_add(1, Ordering::Relaxed);
}

/// A counted fiber context finished (called at most once per [`fiber_started`]).
#[inline]
pub(crate) fn fiber_finished() {
  LIVE.fetch_sub(1, Ordering::Relaxed);
}

/// Fibers built and not yet done (or dropped), process-wide.
pub fn fibers_live() -> u64 {
  LIVE.load(Ordering::Relaxed)
}

/// Fibers built since the process started.
pub fn fibers_started() -> u64 {
  STARTED.load(Ordering::Relaxed)
}

/// The runtime metrics read now: the [`FIBERS_LIVE`] gauge and the [`FIBERS_STARTED_TOTAL`]
/// counter, in that order.
pub fn snapshot() -> Vec<MetricPair> {
  vec![
    MetricPair {
      key: MetricKey::new(FIBERS_LIVE, MetricKind::Gauge),
      state: MetricState::Gauge {
        value: i64::try_from(fibers_live()).unwrap_or(i64::MAX),
      },
    },
    MetricPair {
      key: MetricKey::new(FIBERS_STARTED_TOTAL, MetricKind::Counter),
      state: MetricState::Counter {
        count: fibers_started(),
      },
    },
  ]
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::AtomicUsize;
  use std::time::{Duration, Instant};

  use super::*;
  use crate::{HNil, Never, Runtime, sleep, sync};

  #[test]
  fn blocked_fibers_are_live_and_counted_as_started() {
    const N: u64 = 8;
    let started_before = fibers_started();
    let running = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..N)
      .map(|_| {
        let running = Arc::clone(&running);
        Runtime::global().fork(
          sync::<_, Never, HNil>(move || {
            running.fetch_add(1, Ordering::SeqCst);
          })
          .then(sleep(Duration::from_secs(3600))),
        )
      })
      .collect();
    let deadline = Instant::now() + Duration::from_secs(10);
    while running.load(Ordering::SeqCst) < N as usize {
      assert!(Instant::now() < deadline, "forked fibers never ran");
      std::thread::sleep(Duration::from_millis(1));
    }
    assert!(fibers_live() >= N, "live {} < {N}", fibers_live());
    assert!(fibers_started() >= started_before + N);
    let snap = snapshot();
    assert_eq!(snap[0].key.name(), FIBERS_LIVE);
    assert!(matches!(snap[0].state, MetricState::Gauge { value } if value >= N as i64));
    assert_eq!(snap[1].key.name(), FIBERS_STARTED_TOTAL);
    for h in &handles {
      h.interrupt_now();
    }
    for h in &handles {
      let deadline = Instant::now() + Duration::from_secs(10);
      while !h.fiber().is_done() {
        assert!(
          Instant::now() < deadline,
          "interrupted fiber never finished"
        );
        std::thread::sleep(Duration::from_millis(1));
      }
    }
  }
}
