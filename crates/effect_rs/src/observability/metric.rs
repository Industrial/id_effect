//! Named metrics: counters, gauges, histograms, summaries, frequency, timers.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use crate::collections::hash_map::{self, EffectHashMap};
use crate::kernel::{Effect, box_future};
use crate::runtime::Never;
use crate::scheduling::duration::Duration;

#[derive(Clone, Debug)]
struct Tags {
  name: String,
  pairs: Vec<(String, String)>,
}

#[derive(Clone)]
enum Inner {
  Counter {
    tags: Tags,
    value: Arc<Mutex<u64>>,
  },
  Gauge {
    tags: Tags,
    value: Arc<Mutex<i64>>,
  },
  Histogram {
    tags: Tags,
    obs: Arc<Mutex<Vec<Duration>>>,
  },
  Summary {
    tags: Tags,
    obs: Arc<Mutex<Vec<Duration>>>,
  },
  Frequency {
    tags: Tags,
    counts: Arc<Mutex<EffectHashMap<String, u64>>>,
  },
  Timer {
    tags: Tags,
    obs: Arc<Mutex<Vec<Duration>>>,
  },
}

/// Phantom-typed metric handle; construct via [`Metric::counter`], [`Metric::gauge`], etc.
#[derive(Clone)]
pub struct Metric<In, Out = ()> {
  inner: Inner,
  _p: PhantomData<(In, Out)>,
}

impl<In, Out> Metric<In, Out> {
  fn new(inner: Inner) -> Self {
    Self {
      inner,
      _p: PhantomData,
    }
  }

  /// Metric name set at construction.
  #[inline]
  pub fn name(&self) -> &str {
    match &self.inner {
      Inner::Counter { tags, .. }
      | Inner::Gauge { tags, .. }
      | Inner::Histogram { tags, .. }
      | Inner::Summary { tags, .. }
      | Inner::Frequency { tags, .. }
      | Inner::Timer { tags, .. } => tags.name.as_str(),
    }
  }

  /// Label key/value pairs attached to this metric.
  #[inline]
  pub fn tags(&self) -> &[(String, String)] {
    match &self.inner {
      Inner::Counter { tags, .. }
      | Inner::Gauge { tags, .. }
      | Inner::Histogram { tags, .. }
      | Inner::Summary { tags, .. }
      | Inner::Frequency { tags, .. }
      | Inner::Timer { tags, .. } => tags.pairs.as_slice(),
    }
  }
}

fn tags(name: impl Into<String>, pairs: impl IntoIterator<Item = (String, String)>) -> Tags {
  Tags {
    name: name.into(),
    pairs: pairs.into_iter().collect(),
  }
}

/// Builds a counter (same as [`Metric::counter`]).
pub fn make(
  name: impl Into<String>,
  tag_pairs: impl IntoIterator<Item = (String, String)>,
) -> Metric<u64, ()> {
  Metric::counter(name, tag_pairs)
}

impl Metric<u64, ()> {
  /// New counter metric (initial value zero).
  pub fn counter(
    name: impl Into<String>,
    tag_pairs: impl IntoIterator<Item = (String, String)>,
  ) -> Self {
    Self::new(Inner::Counter {
      tags: tags(name, tag_pairs),
      value: Arc::new(Mutex::new(0)),
    })
  }

  /// Effect that adds `delta` to the counter (saturating).
  pub fn apply(&self, delta: u64) -> Effect<(), Never, ()> {
    let v = Arc::clone(match &self.inner {
      Inner::Counter { value, .. } => value,
      _ => unreachable!("metric kind"),
    });
    Effect::new(move |_env| {
      let mut g = v.lock().expect("mutex");
      *g = g.saturating_add(delta);
      Ok(())
    })
  }

  /// Current counter value (for tests or debugging).
  pub fn snapshot_count(&self) -> u64 {
    let Inner::Counter { value, .. } = &self.inner else {
      unreachable!();
    };
    *value.lock().expect("mutex")
  }
}

impl Metric<i64, ()> {
  /// New gauge metric (initial value zero).
  pub fn gauge(
    name: impl Into<String>,
    tag_pairs: impl IntoIterator<Item = (String, String)>,
  ) -> Self {
    Self::new(Inner::Gauge {
      tags: tags(name, tag_pairs),
      value: Arc::new(Mutex::new(0)),
    })
  }

  /// Effect that sets the gauge to `value`.
  pub fn apply(&self, value: i64) -> Effect<(), Never, ()> {
    let v = Arc::clone(match &self.inner {
      Inner::Gauge { value, .. } => value,
      _ => unreachable!("metric kind"),
    });
    Effect::new(move |_env| {
      let mut g = v.lock().expect("mutex");
      *g = value;
      Ok(())
    })
  }

  /// Current gauge value (for tests or debugging).
  pub fn snapshot_value(&self) -> i64 {
    let Inner::Gauge { value, .. } = &self.inner else {
      unreachable!();
    };
    *value.lock().expect("mutex")
  }
}

impl Metric<Duration, ()> {
  /// Histogram that records raw duration samples.
  pub fn histogram(
    name: impl Into<String>,
    tag_pairs: impl IntoIterator<Item = (String, String)>,
  ) -> Self {
    Self::new(Inner::Histogram {
      tags: tags(name, tag_pairs),
      obs: Arc::new(Mutex::new(Vec::new())),
    })
  }

  /// Summary metric that records duration samples (same storage shape as histogram here).
  pub fn summary(
    name: impl Into<String>,
    tag_pairs: impl IntoIterator<Item = (String, String)>,
  ) -> Self {
    Self::new(Inner::Summary {
      tags: tags(name, tag_pairs),
      obs: Arc::new(Mutex::new(Vec::new())),
    })
  }

  /// Timer metric used with [`Metric::track_duration`] to record elapsed time per effect run.
  pub fn timer(
    name: impl Into<String>,
    tag_pairs: impl IntoIterator<Item = (String, String)>,
  ) -> Self {
    Self::new(Inner::Timer {
      tags: tags(name, tag_pairs),
      obs: Arc::new(Mutex::new(Vec::new())),
    })
  }

  /// Effect that appends one duration observation.
  pub fn apply(&self, d: Duration) -> Effect<(), Never, ()> {
    let obs = Arc::clone(match &self.inner {
      Inner::Histogram { obs, .. } | Inner::Summary { obs, .. } | Inner::Timer { obs, .. } => obs,
      _ => unreachable!("metric kind"),
    });
    Effect::new(move |_env| {
      let mut g = obs.lock().expect("mutex");
      g.push(d);
      Ok(())
    })
  }

  /// Wraps `effect` to record wall-clock duration when the inner effect completes (async).
  pub fn track_duration<A, E, R>(&self, effect: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: 'static,
    E: 'static,
    R: 'static,
  {
    let obs = Arc::clone(match &self.inner {
      Inner::Histogram { obs, .. } | Inner::Summary { obs, .. } | Inner::Timer { obs, .. } => obs,
      _ => unreachable!("metric kind"),
    });
    Effect::new_async(move |env: &mut R| {
      box_future(async move {
        let start = std::time::Instant::now();
        let out = effect.run(env).await;
        let elapsed = start.elapsed();
        if let Ok(mut g) = obs.lock() {
          g.push(elapsed);
        }
        out
      })
    })
  }

  /// Copies all recorded durations (for tests or debugging).
  pub fn snapshot_durations(&self) -> Vec<Duration> {
    let obs = match &self.inner {
      Inner::Histogram { obs, .. } | Inner::Summary { obs, .. } | Inner::Timer { obs, .. } => obs,
      _ => unreachable!("metric kind"),
    };
    obs.lock().expect("mutex").clone()
  }
}

impl Metric<String, ()> {
  /// Frequency / occurrence counter keyed by string labels.
  pub fn frequency(
    name: impl Into<String>,
    tag_pairs: impl IntoIterator<Item = (String, String)>,
  ) -> Self {
    Self::new(Inner::Frequency {
      tags: tags(name, tag_pairs),
      counts: Arc::new(Mutex::new(hash_map::empty())),
    })
  }

  /// Effect that increments the count for `key` (saturating).
  pub fn apply(&self, key: String) -> Effect<(), Never, ()> {
    let counts = Arc::clone(match &self.inner {
      Inner::Frequency { counts, .. } => counts,
      _ => unreachable!("metric kind"),
    });
    Effect::new(move |_env| {
      let mut g = counts.lock().expect("mutex");
      let cur = hash_map::get(&*g, key.as_str()).copied().unwrap_or(0);
      *g = hash_map::set(&*g, key.clone(), cur.saturating_add(1));
      Ok(())
    })
  }

  /// Snapshot of all key counts (for tests or debugging).
  pub fn snapshot_frequencies(&self) -> EffectHashMap<String, u64> {
    let Inner::Frequency { counts, .. } = &self.inner else {
      unreachable!();
    };
    counts.lock().expect("mutex").clone()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::kernel::succeed;
  use crate::runtime::{Never, run_async, run_blocking};

  #[test]
  fn counter_increments_on_each_apply() {
    let m = Metric::counter("c", Vec::<(String, String)>::new());
    run_blocking(m.apply(1), ()).unwrap();
    run_blocking(m.apply(2), ()).unwrap();
    assert_eq!(m.snapshot_count(), 3);
  }

  #[test]
  fn histogram_records_duration() {
    let m = Metric::histogram("h", Vec::<(String, String)>::new());
    run_blocking(m.apply(Duration::from_millis(10)), ()).unwrap();
    run_blocking(m.apply(Duration::from_millis(20)), ()).unwrap();
    let obs = m.snapshot_durations();
    assert_eq!(obs.len(), 2);
  }

  #[test]
  fn frequency_tracks_distinct_values() {
    let m = Metric::frequency("f", Vec::<(String, String)>::new());
    run_blocking(m.apply("a".into()), ()).unwrap();
    run_blocking(m.apply("b".into()), ()).unwrap();
    run_blocking(m.apply("a".into()), ()).unwrap();
    let snap = m.snapshot_frequencies();
    assert_eq!(hash_map::get(&snap, "a"), Some(&2));
    assert_eq!(hash_map::get(&snap, "b"), Some(&1));
  }

  #[tokio::test]
  async fn track_duration_records_elapsed_wall_time() {
    let m = Metric::timer("t", Vec::<(String, String)>::new());
    let eff = m.track_duration(succeed::<u32, Never, ()>(42));
    let v = run_async(eff, ()).await.unwrap();
    assert_eq!(v, 42);
    let obs = m.snapshot_durations();
    assert_eq!(obs.len(), 1);
    assert!(obs[0] > Duration::ZERO);
  }

  // ── make (module-level alias) ─────────────────────────────────────────────

  #[test]
  fn make_creates_counter_with_zero_initial_value() {
    let m = make("requests", Vec::<(String, String)>::new());
    assert_eq!(m.snapshot_count(), 0);
    run_blocking(m.apply(5), ()).unwrap();
    assert_eq!(m.snapshot_count(), 5);
  }

  // ── name / tags ───────────────────────────────────────────────────────────

  #[test]
  fn metric_name_returns_configured_name() {
    let m = Metric::counter("my_counter", Vec::<(String, String)>::new());
    assert_eq!(m.name(), "my_counter");
  }

  #[test]
  fn metric_tags_returns_configured_pairs() {
    let pairs = vec![
      ("region".to_owned(), "us-east".to_owned()),
      ("service".to_owned(), "api".to_owned()),
    ];
    let m = Metric::counter("req", pairs.clone());
    assert_eq!(m.tags(), pairs.as_slice());
  }

  #[test]
  fn metric_tags_empty_when_no_pairs_given() {
    let m = Metric::gauge("g", Vec::<(String, String)>::new());
    assert!(m.tags().is_empty());
  }

  // ── gauge ─────────────────────────────────────────────────────────────────

  #[test]
  fn gauge_apply_sets_value_and_snapshot_returns_it() {
    let m = Metric::gauge("cpu", Vec::<(String, String)>::new());
    assert_eq!(m.snapshot_value(), 0);
    run_blocking(m.apply(75), ()).unwrap();
    assert_eq!(m.snapshot_value(), 75);
    run_blocking(m.apply(-10), ()).unwrap();
    assert_eq!(m.snapshot_value(), -10);
  }

  #[test]
  fn gauge_name_accessible() {
    let m = Metric::gauge("memory_bytes", Vec::<(String, String)>::new());
    assert_eq!(m.name(), "memory_bytes");
  }

  // ── summary ───────────────────────────────────────────────────────────────

  #[test]
  fn summary_records_duration_observations() {
    let m = Metric::summary("latency_p99", Vec::<(String, String)>::new());
    run_blocking(m.apply(Duration::from_millis(5)), ()).unwrap();
    run_blocking(m.apply(Duration::from_millis(15)), ()).unwrap();
    let obs = m.snapshot_durations();
    assert_eq!(obs.len(), 2);
  }

  #[test]
  fn summary_name_accessible() {
    let m = Metric::summary("request_duration", Vec::<(String, String)>::new());
    assert_eq!(m.name(), "request_duration");
  }
}
