//! Metric keys, the five metric kinds (Effect.ts `Metric`) with their lock-free state, the
//! [`Metric`] trait and its `track*` effect combinators. Filled by `p-metrics`.
//!
//! # Model
//!
//! A metric is identified by a [`MetricKey`]: its name, its [`MetricKind`] (including the
//! histogram boundaries or the summary window) and its sorted tags. Every handle
//! ([`Counter`], [`Gauge`], [`Histogram`], [`Summary`], [`Frequency`]) is a cheap clone of a
//! key, the shared state for that key and the [`MetricRegistry`] that owns the state. Building
//! the same key twice in one registry yields handles over the same state, as in Effect.ts.
//! [`Metric::tagged`] returns the handle for the key with one more tag (a distinct series).
//!
//! # State
//!
//! Counters and gauges are one `AtomicU64` / `AtomicI64`; a histogram is one `AtomicU64` per
//! bucket plus CAS-updated `f64` sum / min / max; a frequency is a read-mostly map of
//! `AtomicU64` counters. Only the summary keeps a lock, around its bounded sample window
//! (at most [`SummaryConfig::max_size`] samples, none older than [`SummaryConfig::max_age`]).
//!
//! # Updating
//!
//! [`Metric::update`] (and the inherent `increment` / `set` / `observe` helpers) are effects:
//! they read the running fiber's metric tags ([`Effect::tag_metrics`]) and executor clock
//! (virtual on `TestExecutor`). [`Metric::update_now`] updates outside an effect. The
//! `track_*` methods wrap an effect and update the metric when it ends, however it ends
//! (success, failure, defect or interrupt), from inside a masked region.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

use super::observe_exit_with_fiber;
use super::registry::{MetricRegistry, MetricUpdate};
use crate::failure::Cause;
use crate::kernel::{Effect, ExitKind, Instr, erase};

/// Tag pairs (`key`, `value`), sorted by key with unique keys.
pub type MetricTags = Arc<[(String, String)]>;

/// `base` with `extra` merged in: a key in `extra` replaces the same key in `base`; the result
/// is sorted by key.
pub(crate) fn merge_tags(base: &[(String, String)], extra: &[(String, String)]) -> MetricTags {
  let mut out: Vec<(String, String)> = base.to_vec();
  for (k, v) in extra {
    match out.iter_mut().find(|(ok, _)| ok == k) {
      Some(slot) => slot.1.clone_from(v),
      None => out.push((k.clone(), v.clone())),
    }
  }
  out.sort_by(|a, b| a.0.cmp(&b.0));
  out.into()
}

// ── keys ────────────────────────────────────────────────────────────────────────────────────

/// Histogram bucket upper bounds: finite, strictly increasing. A histogram over `n` boundaries
/// has `n + 1` buckets; the last one (`+inf`) takes every sample above the last boundary.
#[derive(Clone, Debug)]
pub struct Boundaries(Arc<[f64]>);

impl Boundaries {
  /// The given bounds, with non-finite values dropped, sorted and deduplicated.
  pub fn new(bounds: impl IntoIterator<Item = f64>) -> Self {
    let mut v: Vec<f64> = bounds.into_iter().filter(|b| b.is_finite()).collect();
    v.sort_by(f64::total_cmp);
    v.dedup();
    Self(v.into())
  }

  /// `count` bounds `start, start + width, ..` (Effect.ts `MetricBoundaries.linear`).
  pub fn linear(start: f64, width: f64, count: usize) -> Self {
    Self::new((0..count).map(|i| start + width * i as f64))
  }

  /// `count` bounds `start, start * factor, start * factor², ..` (Effect.ts
  /// `MetricBoundaries.exponential`).
  pub fn exponential(start: f64, factor: f64, count: usize) -> Self {
    Self::new((0..count).map(|i| start * factor.powi(i as i32)))
  }

  /// The finite upper bounds (without the implicit `+inf`).
  pub fn as_slice(&self) -> &[f64] {
    &self.0
  }

  /// The number of finite bounds.
  pub fn len(&self) -> usize {
    self.0.len()
  }

  /// `true` when there are no finite bounds (a single `+inf` bucket).
  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
}

impl PartialEq for Boundaries {
  fn eq(&self, other: &Self) -> bool {
    self.0.len() == other.0.len()
      && self
        .0
        .iter()
        .zip(other.0.iter())
        .all(|(a, b)| a.to_bits() == b.to_bits())
  }
}

impl Eq for Boundaries {}

impl Hash for Boundaries {
  fn hash<H: Hasher>(&self, state: &mut H) {
    for b in self.0.iter() {
      b.to_bits().hash(state);
    }
  }
}

/// A summary's sample window and reported quantiles (Effect.ts `Metric.summary`).
#[derive(Clone, Debug)]
pub struct SummaryConfig {
  max_age: Duration,
  max_size: usize,
  quantiles: Arc<[f64]>,
}

impl SummaryConfig {
  /// Keeps at most `max_size` samples (at least 1), none older than `max_age`, and reports the
  /// given quantiles (values outside `[0, 1]` dropped, sorted, deduplicated).
  pub fn new(max_age: Duration, max_size: usize, quantiles: impl IntoIterator<Item = f64>) -> Self {
    let mut q: Vec<f64> = quantiles
      .into_iter()
      .filter(|q| (0.0..=1.0).contains(q))
      .collect();
    q.sort_by(f64::total_cmp);
    q.dedup();
    Self {
      max_age,
      max_size: max_size.max(1),
      quantiles: q.into(),
    }
  }

  /// The oldest sample age kept in the window.
  pub fn max_age(&self) -> Duration {
    self.max_age
  }

  /// The most samples kept in the window.
  pub fn max_size(&self) -> usize {
    self.max_size
  }

  /// The reported quantiles.
  pub fn quantiles(&self) -> &[f64] {
    &self.quantiles
  }
}

impl PartialEq for SummaryConfig {
  fn eq(&self, other: &Self) -> bool {
    self.max_age == other.max_age
      && self.max_size == other.max_size
      && self.quantiles.len() == other.quantiles.len()
      && self
        .quantiles
        .iter()
        .zip(other.quantiles.iter())
        .all(|(a, b)| a.to_bits() == b.to_bits())
  }
}

impl Eq for SummaryConfig {}

impl Hash for SummaryConfig {
  fn hash<H: Hasher>(&self, state: &mut H) {
    self.max_age.hash(state);
    self.max_size.hash(state);
    for q in self.quantiles.iter() {
      q.to_bits().hash(state);
    }
  }
}

/// The kind of a metric, part of its key (Effect.ts `MetricKeyType`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MetricKind {
  /// A monotonic `u64` counter.
  Counter,
  /// An `i64` gauge (last value set).
  Gauge,
  /// A histogram over fixed bucket boundaries.
  Histogram(Boundaries),
  /// A summary over a bounded sample window.
  Summary(SummaryConfig),
  /// Occurrence counts per string value.
  Frequency,
}

impl MetricKind {
  /// A short, stable label: `counter`, `gauge`, `histogram`, `summary` or `frequency`.
  pub fn label(&self) -> &'static str {
    match self {
      Self::Counter => "counter",
      Self::Gauge => "gauge",
      Self::Histogram(_) => "histogram",
      Self::Summary(_) => "summary",
      Self::Frequency => "frequency",
    }
  }
}

/// A metric's identity: name, kind and sorted tags (Effect.ts `MetricKey`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MetricKey {
  name: Arc<str>,
  kind: MetricKind,
  tags: MetricTags,
}

impl MetricKey {
  /// A key with no tags.
  pub fn new(name: impl Into<Arc<str>>, kind: MetricKind) -> Self {
    Self {
      name: name.into(),
      kind,
      tags: Arc::from(Vec::new()),
    }
  }

  /// The metric name.
  pub fn name(&self) -> &str {
    &self.name
  }

  /// The metric kind.
  pub fn kind(&self) -> &MetricKind {
    &self.kind
  }

  /// The tags, sorted by key.
  pub fn tags(&self) -> &[(String, String)] {
    &self.tags
  }

  /// This key with `extra` merged into its tags (a key in `extra` wins).
  pub fn with_tags(&self, extra: &[(String, String)]) -> Self {
    Self {
      name: Arc::clone(&self.name),
      kind: self.kind.clone(),
      tags: merge_tags(&self.tags, extra),
    }
  }
}

// ── lock-free state ─────────────────────────────────────────────────────────────────────────

/// Replaces the value with `f(value)` by a CAS loop until it sticks; `None` leaves it as is.
fn cas_u64(a: &AtomicU64, f: impl Fn(u64) -> Option<u64>) {
  let mut cur = a.load(Ordering::Relaxed);
  while let Some(next) = f(cur) {
    match a.compare_exchange_weak(cur, next, Ordering::Relaxed, Ordering::Relaxed) {
      Ok(_) => return,
      Err(seen) => cur = seen,
    }
  }
}

/// An `f64` in an `AtomicU64` (bit pattern), updated by CAS.
#[derive(Debug)]
struct AtomicF64(AtomicU64);

impl AtomicF64 {
  fn new(v: f64) -> Self {
    Self(AtomicU64::new(v.to_bits()))
  }

  fn get(&self) -> f64 {
    f64::from_bits(self.0.load(Ordering::Relaxed))
  }

  fn update(&self, f: impl Fn(f64) -> Option<f64>) {
    cas_u64(&self.0, |b| f(f64::from_bits(b)).map(f64::to_bits));
  }

  fn add(&self, v: f64) {
    self.update(|cur| Some(cur + v));
  }

  fn min(&self, v: f64) {
    self.update(|cur| (v < cur).then_some(v));
  }

  fn max(&self, v: f64) {
    self.update(|cur| (v > cur).then_some(v));
  }
}

/// Sum / min / max over the samples of a histogram or summary.
#[derive(Debug)]
struct Moments {
  count: AtomicU64,
  sum: AtomicF64,
  min: AtomicF64,
  max: AtomicF64,
}

impl Moments {
  fn new() -> Self {
    Self {
      count: AtomicU64::new(0),
      sum: AtomicF64::new(0.0),
      min: AtomicF64::new(f64::INFINITY),
      max: AtomicF64::new(f64::NEG_INFINITY),
    }
  }

  fn record(&self, v: f64) {
    self.count.fetch_add(1, Ordering::Relaxed);
    self.sum.add(v);
    self.min.min(v);
    self.max.max(v);
  }

  fn read(&self) -> (u64, f64, Option<f64>, Option<f64>) {
    let count = self.count.load(Ordering::Relaxed);
    if count == 0 {
      (0, 0.0, None, None)
    } else {
      (
        count,
        self.sum.get(),
        Some(self.min.get()),
        Some(self.max.get()),
      )
    }
  }
}

#[derive(Debug)]
pub(crate) struct CounterState(AtomicU64);

impl CounterState {
  fn add(&self, n: u64) {
    cas_u64(&self.0, |v| Some(v.saturating_add(n)));
  }
}

#[derive(Debug)]
pub(crate) struct GaugeState(AtomicI64);

impl GaugeState {
  /// Adds `delta` (saturating) and returns the new value.
  fn adjust(&self, delta: i64) -> i64 {
    let mut cur = self.0.load(Ordering::Relaxed);
    loop {
      let next = cur.saturating_add(delta);
      match self
        .0
        .compare_exchange_weak(cur, next, Ordering::Relaxed, Ordering::Relaxed)
      {
        Ok(_) => return next,
        Err(seen) => cur = seen,
      }
    }
  }
}

#[derive(Debug)]
pub(crate) struct HistogramState {
  bounds: Boundaries,
  buckets: Box<[AtomicU64]>,
  moments: Moments,
}

impl HistogramState {
  fn new(bounds: Boundaries) -> Self {
    let buckets = (0..=bounds.len()).map(|_| AtomicU64::new(0)).collect();
    Self {
      bounds,
      buckets,
      moments: Moments::new(),
    }
  }

  /// Records `v` in the first bucket whose bound is `>= v` (the `+inf` bucket past the last);
  /// `false` (nothing recorded) for NaN.
  fn observe(&self, v: f64) -> bool {
    if v.is_nan() {
      return false;
    }
    let i = self.bounds.as_slice().partition_point(|b| *b < v);
    self.buckets[i].fetch_add(1, Ordering::Relaxed);
    self.moments.record(v);
    true
  }

  fn snapshot(&self) -> HistogramSnapshot {
    let buckets: Vec<(f64, u64)> = self
      .bounds
      .as_slice()
      .iter()
      .copied()
      .chain(std::iter::once(f64::INFINITY))
      .zip(self.buckets.iter().map(|c| c.load(Ordering::Relaxed)))
      .collect();
    let (_, sum, min, max) = self.moments.read();
    HistogramSnapshot {
      count: buckets.iter().map(|(_, c)| c).sum(),
      buckets,
      sum,
      min,
      max,
    }
  }
}

#[derive(Debug)]
pub(crate) struct SummaryState {
  config: SummaryConfig,
  window: Mutex<VecDeque<(Instant, f64)>>,
  moments: Moments,
}

impl SummaryState {
  fn new(config: SummaryConfig) -> Self {
    Self {
      window: Mutex::new(VecDeque::with_capacity(config.max_size.min(1024))),
      config,
      moments: Moments::new(),
    }
  }

  fn expired(&self, at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(at) > self.config.max_age
  }

  fn prune(&self, w: &mut VecDeque<(Instant, f64)>, now: Instant) {
    while w.len() > self.config.max_size {
      w.pop_front();
    }
    while w.front().is_some_and(|(at, _)| self.expired(*at, now)) {
      w.pop_front();
    }
  }

  fn observe(&self, v: f64, now: Instant) -> bool {
    if v.is_nan() {
      return false;
    }
    let mut w = self.window.lock().unwrap_or_else(PoisonError::into_inner);
    w.push_back((now, v));
    self.prune(&mut w, now);
    drop(w);
    self.moments.record(v);
    true
  }

  fn snapshot(&self, now: Instant) -> SummarySnapshot {
    let mut values: Vec<f64> = {
      let mut w = self.window.lock().unwrap_or_else(PoisonError::into_inner);
      self.prune(&mut w, now);
      w.iter()
        .filter(|(at, _)| !self.expired(*at, now))
        .map(|(_, v)| *v)
        .collect()
    };
    values.sort_by(f64::total_cmp);
    let quantiles = self
      .config
      .quantiles
      .iter()
      .map(|&q| (q, nearest_rank(&values, q)))
      .collect();
    let (count, sum, min, max) = self.moments.read();
    SummarySnapshot {
      quantiles,
      window_len: values.len(),
      count,
      sum,
      min,
      max,
    }
  }
}

/// The nearest-rank `q` quantile of sorted `values` (`None` when empty).
fn nearest_rank(values: &[f64], q: f64) -> Option<f64> {
  if values.is_empty() {
    return None;
  }
  let rank = (q * values.len() as f64).ceil() as usize;
  Some(values[rank.clamp(1, values.len()) - 1])
}

#[derive(Debug, Default)]
pub(crate) struct FrequencyState(RwLock<HashMap<String, AtomicU64>>);

impl FrequencyState {
  fn increment(&self, key: &str) {
    {
      let m = self.0.read().unwrap_or_else(PoisonError::into_inner);
      if let Some(c) = m.get(key) {
        c.fetch_add(1, Ordering::Relaxed);
        return;
      }
    }
    let mut m = self.0.write().unwrap_or_else(PoisonError::into_inner);
    m.entry(key.to_owned())
      .or_default()
      .fetch_add(1, Ordering::Relaxed);
  }

  fn snapshot(&self) -> Vec<(String, u64)> {
    let m = self.0.read().unwrap_or_else(PoisonError::into_inner);
    let mut v: Vec<(String, u64)> = m
      .iter()
      .map(|(k, c)| (k.clone(), c.load(Ordering::Relaxed)))
      .collect();
    drop(m);
    v.sort();
    v
  }
}

/// The state behind one key in a registry.
#[derive(Clone, Debug)]
pub(crate) enum MetricCell {
  Counter(Arc<CounterState>),
  Gauge(Arc<GaugeState>),
  Histogram(Arc<HistogramState>),
  Summary(Arc<SummaryState>),
  Frequency(Arc<FrequencyState>),
}

impl MetricCell {
  /// Fresh state for `kind`.
  pub(crate) fn for_kind(kind: &MetricKind) -> Self {
    match kind {
      MetricKind::Counter => Self::Counter(Arc::new(CounterState(AtomicU64::new(0)))),
      MetricKind::Gauge => Self::Gauge(Arc::new(GaugeState(AtomicI64::new(0)))),
      MetricKind::Histogram(b) => Self::Histogram(Arc::new(HistogramState::new(b.clone()))),
      MetricKind::Summary(c) => Self::Summary(Arc::new(SummaryState::new(c.clone()))),
      MetricKind::Frequency => Self::Frequency(Arc::default()),
    }
  }

  /// A point-in-time reading (`now` ages the summary window).
  pub(crate) fn snapshot(&self, now: Instant) -> MetricState {
    match self {
      Self::Counter(s) => MetricState::Counter {
        count: s.0.load(Ordering::Relaxed),
      },
      Self::Gauge(s) => MetricState::Gauge {
        value: s.0.load(Ordering::Relaxed),
      },
      Self::Histogram(s) => MetricState::Histogram(s.snapshot()),
      Self::Summary(s) => MetricState::Summary(s.snapshot(now)),
      Self::Frequency(s) => MetricState::Frequency {
        occurrences: s.snapshot(),
      },
    }
  }
}

// ── snapshots ───────────────────────────────────────────────────────────────────────────────

/// A histogram reading.
#[derive(Clone, Debug, PartialEq)]
pub struct HistogramSnapshot {
  /// `(upper bound, samples in the bucket)` per bucket (not cumulative); the last bound is
  /// `f64::INFINITY`, so there are `boundaries + 1` entries.
  pub buckets: Vec<(f64, u64)>,
  /// The number of samples (the sum of the bucket counts).
  pub count: u64,
  /// The sum of the samples.
  pub sum: f64,
  /// The smallest sample (`None` before the first).
  pub min: Option<f64>,
  /// The largest sample (`None` before the first).
  pub max: Option<f64>,
}

/// A summary reading.
#[derive(Clone, Debug, PartialEq)]
pub struct SummarySnapshot {
  /// `(quantile, value)` over the current window (`None` when the window is empty).
  pub quantiles: Vec<(f64, Option<f64>)>,
  /// The number of samples in the window (at most `max_size`).
  pub window_len: usize,
  /// The number of samples ever recorded.
  pub count: u64,
  /// The sum of every sample ever recorded.
  pub sum: f64,
  /// The smallest sample ever recorded (`None` before the first).
  pub min: Option<f64>,
  /// The largest sample ever recorded (`None` before the first).
  pub max: Option<f64>,
}

/// A metric reading (Effect.ts `MetricState`).
#[derive(Clone, Debug, PartialEq)]
pub enum MetricState {
  /// A counter's total.
  Counter {
    /// The total.
    count: u64,
  },
  /// A gauge's value.
  Gauge {
    /// The value.
    value: i64,
  },
  /// A histogram reading.
  Histogram(HistogramSnapshot),
  /// A summary reading.
  Summary(SummarySnapshot),
  /// A frequency reading.
  Frequency {
    /// `(value, occurrences)`, sorted by value.
    occurrences: Vec<(String, u64)>,
  },
}

// ── the Metric trait ────────────────────────────────────────────────────────────────────────

/// An effect that runs `f(fiber_tags, now)` as a synchronous step of the running fiber (a panic
/// in `f`, e.g. in a listener, becomes `Die`).
fn metric_step<E, R>(
  f: impl FnOnce(&[(String, String)], Instant) + Send + 'static,
) -> Effect<(), E, R>
where
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
    let tags = MetricRegistry::fiber_tags().get_in(fiber);
    let now = fiber.executor().now();
    Instr::Sync(Box::new(move |_| {
      f(&tags, now);
      Instr::Pure(Ok(erase(())))
    }))
  })))
}

/// The first typed failure in `c` (depth-first).
fn first_fail<E>(c: &Cause<E>) -> Option<&E> {
  match c {
    Cause::Fail(e) => Some(e),
    Cause::Both(l, r) | Cause::Then(l, r) => first_fail(l).or_else(|| first_fail(r)),
    _ => None,
  }
}

/// A metric handle (Effect.ts `Metric<Type, In, Out>`): updated with `Input` values.
///
/// The required methods are the raw surface (used by runtime integrations); the provided
/// methods are the Effect.ts combinators. Every effect-level update merges the running fiber's
/// metric tags ([`Effect::tag_metrics`]) into the key, and reads time from the fiber's executor
/// clock.
pub trait Metric: Clone + Send + Sync + 'static {
  /// The value an update takes.
  type Input: Send + 'static;

  /// This metric's key.
  fn key(&self) -> &MetricKey;

  /// The registry holding this metric's state.
  fn registry(&self) -> &MetricRegistry;

  /// The handle for this key with `tags` merged in (a distinct series in the same registry).
  fn tagged_all(&self, tags: &[(String, String)]) -> Self;

  /// Records `input` at `now` outside an effect, ignoring fiber tags.
  fn record(&self, input: Self::Input, now: Instant);

  /// A reading of this metric now.
  fn state(&self) -> MetricState;

  /// The metric name.
  fn name(&self) -> &str {
    self.key().name()
  }

  /// The metric tags, sorted by key.
  fn tags(&self) -> &[(String, String)] {
    self.key().tags()
  }

  /// The handle with tag `key = value` added (Effect.ts `Metric.tagged`).
  fn tagged(&self, key: impl Into<String>, value: impl Into<String>) -> Self {
    self.tagged_all(&[(key.into(), value.into())])
  }

  /// Records `input` now, outside an effect (Effect.ts `unsafeUpdate`).
  fn update_now(&self, input: Self::Input) {
    self.record(input, Instant::now());
  }

  /// Records `input` in the running fiber (Effect.ts `Metric.update`).
  fn update<E, R>(&self, input: Self::Input) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let m = self.clone();
    metric_step(move |tags, now| m.record_tagged(tags, input, now))
  }

  /// Records `input` under the key with `fiber_tags` merged in.
  fn record_tagged(&self, fiber_tags: &[(String, String)], input: Self::Input, now: Instant) {
    if fiber_tags.is_empty() {
      self.record(input, now);
    } else {
      self.tagged_all(fiber_tags).record(input, now);
    }
  }

  /// Runs `eff`, then records `f(kind)` when it returns `Some`, however `eff` ends.
  fn track_exit_with<A, E, R>(
    &self,
    eff: Effect<A, E, R>,
    f: impl FnOnce(ExitKind) -> Option<Self::Input> + Send + 'static,
  ) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let m = self.clone();
    observe_exit_with_fiber(eff, move |exit, fiber| {
      if let Some(input) = f(ExitKind::of_result(exit)) {
        let tags = MetricRegistry::fiber_tags().get_in(fiber);
        m.record_tagged(&tags, input, fiber.executor().now());
      }
    })
  }

  /// Runs `eff` and records `input` once it ends, however it ends (Effect.ts
  /// `Metric.trackAll`).
  fn track_all<A, E, R>(&self, input: Self::Input, eff: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.track_exit_with(eff, move |_| Some(input))
  }

  /// Runs `eff` and records `f(&value)` when it succeeds (Effect.ts `Metric.trackSuccessWith`).
  fn track_success_with<A, E, R>(
    &self,
    eff: Effect<A, E, R>,
    f: impl FnOnce(&A) -> Self::Input + Send + 'static,
  ) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let m = self.clone();
    observe_exit_with_fiber(eff, move |exit, fiber| {
      if let Ok(a) = exit {
        let tags = MetricRegistry::fiber_tags().get_in(fiber);
        m.record_tagged(&tags, f(a), fiber.executor().now());
      }
    })
  }

  /// Runs `eff` and records its success value (Effect.ts: applying a metric to an effect).
  fn track<E, R>(&self, eff: Effect<Self::Input, E, R>) -> Effect<Self::Input, E, R>
  where
    Self::Input: Clone,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.track_success_with(eff, Clone::clone)
  }

  /// Runs `eff` and records `f(&error)` when it fails with a typed error (Effect.ts
  /// `Metric.trackErrorWith`). Defects and interrupts record nothing.
  fn track_error_with<A, E, R>(
    &self,
    eff: Effect<A, E, R>,
    f: impl FnOnce(&E) -> Self::Input + Send + 'static,
  ) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let m = self.clone();
    observe_exit_with_fiber(eff, move |exit, fiber| {
      if let Some(e) = exit.as_ref().err().and_then(first_fail) {
        let tags = MetricRegistry::fiber_tags().get_in(fiber);
        m.record_tagged(&tags, f(e), fiber.executor().now());
      }
    })
  }

  /// Runs `eff` and records `f(elapsed)` once it ends, however it ends; `elapsed` is measured on
  /// the fiber's executor clock (Effect.ts `Metric.trackDurationWith`).
  fn track_duration_with<A, E, R>(
    &self,
    eff: Effect<A, E, R>,
    f: impl FnOnce(Duration) -> Self::Input + Send + 'static,
  ) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let m = self.clone();
    Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
      let start = fiber.executor().now();
      observe_exit_with_fiber(eff, move |_, fiber| {
        let now = fiber.executor().now();
        let tags = MetricRegistry::fiber_tags().get_in(fiber);
        m.record_tagged(&tags, f(now.saturating_duration_since(start)), now);
      })
      .into_instr()
    })))
  }
}

// ── handles ─────────────────────────────────────────────────────────────────────────────────

macro_rules! handle_common {
  ($ty:ident, $variant:ident, $state:ty) => {
    impl $ty {
      /// The handle for `key` in `registry`. The key gets state (and shows up in snapshots) on
      /// first use, so intermediate handles such as `base` in `base.tagged(..)` stay invisible.
      pub(crate) fn from_key(registry: &MetricRegistry, key: MetricKey) -> Self {
        Self {
          key,
          state: Arc::new(OnceLock::new()),
          registry: registry.clone(),
        }
      }

      fn st(&self) -> &$state {
        self
          .state
          .get_or_init(|| match self.registry.cell(&self.key) {
            MetricCell::$variant(state) => state,
            _ => unreachable!("id_effect: metric key kind and cell kind disagree"),
          })
      }
    }

    impl std::fmt::Debug for $ty {
      fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(stringify!($ty))
          .field("name", &self.key.name())
          .field("tags", &self.key.tags())
          .finish()
      }
    }
  };
}

macro_rules! metric_impl {
  ($ty:ident, $input:ty, |$this:ident, $inp:ident, $now:ident| $record:block) => {
    impl Metric for $ty {
      type Input = $input;

      fn key(&self) -> &MetricKey {
        &self.key
      }

      fn registry(&self) -> &MetricRegistry {
        &self.registry
      }

      fn tagged_all(&self, tags: &[(String, String)]) -> Self {
        Self::from_key(&self.registry, self.key.with_tags(tags))
      }

      fn record(&self, $inp: $input, $now: Instant) {
        let $this = self;
        $record
      }

      fn state(&self) -> MetricState {
        self.registry.cell(&self.key).snapshot(Instant::now())
      }
    }
  };
}

/// A monotonic `u64` counter (Effect.ts `Metric.counter`). Updates add (saturating).
#[derive(Clone)]
pub struct Counter {
  key: MetricKey,
  state: Arc<OnceLock<Arc<CounterState>>>,
  registry: MetricRegistry,
}

handle_common!(Counter, Counter, CounterState);
metric_impl!(Counter, u64, |this, n, _now| {
  this.st().add(n);
  this.registry.notify(&this.key, MetricUpdate::Counter(n));
});

impl Counter {
  /// The counter `name` in the global registry.
  pub fn new(name: impl Into<Arc<str>>) -> Self {
    MetricRegistry::global().counter(name)
  }

  /// The current total.
  pub fn count(&self) -> u64 {
    self.st().0.load(Ordering::Relaxed)
  }

  /// Adds 1 in the running fiber.
  pub fn increment<E: Send + 'static, R: Send + Sync + 'static>(&self) -> Effect<(), E, R> {
    self.update(1)
  }

  /// Adds `n` in the running fiber.
  pub fn increment_by<E: Send + 'static, R: Send + Sync + 'static>(
    &self,
    n: u64,
  ) -> Effect<(), E, R> {
    self.update(n)
  }
}

/// An `i64` gauge (Effect.ts `Metric.gauge`). Updates set the value.
#[derive(Clone)]
pub struct Gauge {
  key: MetricKey,
  state: Arc<OnceLock<Arc<GaugeState>>>,
  registry: MetricRegistry,
}

handle_common!(Gauge, Gauge, GaugeState);
metric_impl!(Gauge, i64, |this, v, _now| {
  this.st().0.store(v, Ordering::Relaxed);
  this.registry.notify(&this.key, MetricUpdate::Gauge(v));
});

impl Gauge {
  /// The gauge `name` in the global registry.
  pub fn new(name: impl Into<Arc<str>>) -> Self {
    MetricRegistry::global().gauge(name)
  }

  /// The current value.
  pub fn value(&self) -> i64 {
    self.st().0.load(Ordering::Relaxed)
  }

  /// Sets the value in the running fiber.
  pub fn set<E: Send + 'static, R: Send + Sync + 'static>(&self, v: i64) -> Effect<(), E, R> {
    self.update(v)
  }

  /// Adds `delta` (saturating) outside an effect, ignoring fiber tags; returns the new value.
  pub fn adjust_now(&self, delta: i64) -> i64 {
    let v = self.st().adjust(delta);
    self.registry.notify(&self.key, MetricUpdate::Gauge(v));
    v
  }

  /// Adds `delta` (saturating) in the running fiber (Effect.ts `Metric.increment` /
  /// `incrementBy` on a gauge).
  pub fn adjust<E: Send + 'static, R: Send + Sync + 'static>(
    &self,
    delta: i64,
  ) -> Effect<(), E, R> {
    let m = self.clone();
    metric_step(move |tags, _| {
      if tags.is_empty() {
        m.adjust_now(delta);
      } else {
        m.tagged_all(tags).adjust_now(delta);
      }
    })
  }
}

/// A histogram over fixed [`Boundaries`] (Effect.ts `Metric.histogram`). NaN samples are
/// ignored.
#[derive(Clone)]
pub struct Histogram {
  key: MetricKey,
  state: Arc<OnceLock<Arc<HistogramState>>>,
  registry: MetricRegistry,
}

handle_common!(Histogram, Histogram, HistogramState);
metric_impl!(Histogram, f64, |this, v, _now| {
  if this.st().observe(v) {
    this.registry.notify(&this.key, MetricUpdate::Histogram(v));
  }
});

impl Histogram {
  /// The histogram `name` over `boundaries` in the global registry.
  pub fn new(name: impl Into<Arc<str>>, boundaries: Boundaries) -> Self {
    MetricRegistry::global().histogram(name, boundaries)
  }

  /// The bucket boundaries.
  pub fn boundaries(&self) -> &Boundaries {
    &self.st().bounds
  }

  /// A reading of the buckets and moments.
  pub fn snapshot(&self) -> HistogramSnapshot {
    self.st().snapshot()
  }

  /// Records `v` in the running fiber.
  pub fn observe<E: Send + 'static, R: Send + Sync + 'static>(&self, v: f64) -> Effect<(), E, R> {
    self.update(v)
  }

  /// Runs `eff` and records its duration in seconds (executor clock) however it ends
  /// (Effect.ts `Metric.trackDuration`).
  pub fn track_duration<A, E, R>(&self, eff: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.track_duration_with(eff, |d| d.as_secs_f64())
  }
}

/// A summary over a bounded sample window (Effect.ts `Metric.summary`): at most
/// [`SummaryConfig::max_size`] samples, none older than [`SummaryConfig::max_age`]. NaN
/// samples are ignored.
#[derive(Clone)]
pub struct Summary {
  key: MetricKey,
  state: Arc<OnceLock<Arc<SummaryState>>>,
  registry: MetricRegistry,
}

handle_common!(Summary, Summary, SummaryState);
metric_impl!(Summary, f64, |this, v, now| {
  if this.st().observe(v, now) {
    this.registry.notify(&this.key, MetricUpdate::Summary(v));
  }
});

impl Summary {
  /// The summary `name` with `config` in the global registry.
  pub fn new(name: impl Into<Arc<str>>, config: SummaryConfig) -> Self {
    MetricRegistry::global().summary(name, config)
  }

  /// The window and quantile configuration.
  pub fn config(&self) -> &SummaryConfig {
    &self.st().config
  }

  /// A reading with the window aged to `now`.
  pub fn snapshot_at(&self, now: Instant) -> SummarySnapshot {
    self.st().snapshot(now)
  }

  /// A reading with the window aged to the wall clock.
  pub fn snapshot(&self) -> SummarySnapshot {
    self.snapshot_at(Instant::now())
  }

  /// Records `v` in the running fiber.
  pub fn observe<E: Send + 'static, R: Send + Sync + 'static>(&self, v: f64) -> Effect<(), E, R> {
    self.update(v)
  }

  /// Runs `eff` and records its duration in seconds (executor clock) however it ends.
  pub fn track_duration<A, E, R>(&self, eff: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.track_duration_with(eff, |d| d.as_secs_f64())
  }
}

/// Occurrence counts per string value (Effect.ts `Metric.frequency`).
#[derive(Clone)]
pub struct Frequency {
  key: MetricKey,
  state: Arc<OnceLock<Arc<FrequencyState>>>,
  registry: MetricRegistry,
}

handle_common!(Frequency, Frequency, FrequencyState);
metric_impl!(Frequency, String, |this, v, _now| {
  this.st().increment(&v);
  this.registry.notify(&this.key, MetricUpdate::Frequency(&v));
});

impl Frequency {
  /// The frequency `name` in the global registry.
  pub fn new(name: impl Into<Arc<str>>) -> Self {
    MetricRegistry::global().frequency(name)
  }

  /// `(value, occurrences)`, sorted by value.
  pub fn occurrences(&self) -> Vec<(String, u64)> {
    self.st().snapshot()
  }

  /// Counts one occurrence of `value` in the running fiber.
  pub fn occur<E: Send + 'static, R: Send + Sync + 'static>(
    &self,
    value: impl Into<String>,
  ) -> Effect<(), E, R> {
    self.update(value.into())
  }
}
