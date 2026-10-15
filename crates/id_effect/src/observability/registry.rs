//! `MetricRegistry`: the keyed metric store, its tagged snapshots, the push listener hook and
//! the fiber metric tags.
//!
//! A registry maps each [`MetricKey`] to its state. [`MetricRegistry::global`] is the process
//! registry (`Counter::new` and friends); [`MetricRegistry::new`] makes an isolated one (tests,
//! embedded runtimes). Handles carry their registry, so a tagged variant of a metric lives in
//! the same registry as the metric.
//!
//! Two bridge surfaces for exporters (the otel crate uses both):
//!
//! - pull: [`MetricRegistry::snapshot`] returns every key (name, kind, tags) with its
//!   [`MetricState`], sorted by name, kind and tags;
//! - push: a [`MetricListener`] added with [`MetricRegistry::add_listener`] sees each new key
//!   ([`on_register`](MetricListener::on_register)) and each recorded update
//!   ([`on_update`](MetricListener::on_update)), on the updating thread. With no listener the
//!   update path costs one atomic load.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};
use std::time::Instant;

use super::metric::{
  Boundaries, Counter, Frequency, Gauge, Histogram, MetricCell, MetricKey, MetricKind, MetricState,
  MetricTags, Summary, SummaryConfig,
};
use crate::runtime::FiberRef;

/// One recorded update, as a [`MetricListener`] sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MetricUpdate<'a> {
  /// A counter grew by this amount.
  Counter(u64),
  /// A gauge now holds this value (after a set or an adjust).
  Gauge(i64),
  /// A histogram sample.
  Histogram(f64),
  /// A summary sample.
  Summary(f64),
  /// One occurrence of this value.
  Frequency(&'a str),
}

/// A push hook on a registry (the otel bridge's synchronous instruments).
///
/// Called on the thread that records the update, after the state changed; keep it cheap and
/// non-blocking. A panic in a listener inside an effect becomes that effect's `Die`.
pub trait MetricListener: Send + Sync + 'static {
  /// A key got state in the registry (first use of that name, kind and tags).
  fn on_register(&self, _key: &MetricKey) {}

  /// `key` recorded `update`.
  fn on_update(&self, key: &MetricKey, update: MetricUpdate<'_>);
}

/// A key with its reading, as returned by [`MetricRegistry::snapshot`] (Effect.ts
/// `MetricPair`).
#[derive(Clone, Debug, PartialEq)]
pub struct MetricPair {
  /// The metric's name, kind and tags.
  pub key: MetricKey,
  /// The reading.
  pub state: MetricState,
}

type Listeners = Arc<[Arc<dyn MetricListener>]>;

struct RegistryInner {
  cells: RwLock<HashMap<MetricKey, MetricCell>>,
  listeners: RwLock<Listeners>,
  has_listeners: AtomicBool,
}

/// The keyed metric store (Effect.ts `MetricRegistry`). Cloning shares the store.
#[derive(Clone)]
pub struct MetricRegistry {
  inner: Arc<RegistryInner>,
}

impl Default for MetricRegistry {
  fn default() -> Self {
    Self::new()
  }
}

impl std::fmt::Debug for MetricRegistry {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("MetricRegistry")
      .field("metrics", &self.len())
      .finish()
  }
}

impl MetricRegistry {
  /// An empty, isolated registry.
  pub fn new() -> Self {
    Self {
      inner: Arc::new(RegistryInner {
        cells: RwLock::new(HashMap::new()),
        listeners: RwLock::new(Arc::from(Vec::new())),
        has_listeners: AtomicBool::new(false),
      }),
    }
  }

  /// The process registry used by `Counter::new`, `Gauge::new`, ...
  pub fn global() -> &'static MetricRegistry {
    static GLOBAL: OnceLock<MetricRegistry> = OnceLock::new();
    GLOBAL.get_or_init(MetricRegistry::new)
  }

  /// The fiber-local metric tags (Effect.ts `FiberRef.currentMetricLabels`): merged into the key
  /// of every effect-level update in that fiber, inherited by forked children. Set it with
  /// [`Effect::tag_metrics`](crate::Effect::tag_metrics); read it with
  /// [`FiberRef::get_in`] from a runtime integration.
  pub fn fiber_tags() -> &'static FiberRef<MetricTags> {
    static TAGS: OnceLock<FiberRef<MetricTags>> = OnceLock::new();
    TAGS.get_or_init(|| FiberRef::new(Arc::from(Vec::new())))
  }

  /// `true` when both handles share one store.
  pub fn ptr_eq(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.inner, &other.inner)
  }

  /// The counter `name` (no tags).
  pub fn counter(&self, name: impl Into<Arc<str>>) -> Counter {
    Counter::from_key(self, MetricKey::new(name, MetricKind::Counter))
  }

  /// The gauge `name` (no tags).
  pub fn gauge(&self, name: impl Into<Arc<str>>) -> Gauge {
    Gauge::from_key(self, MetricKey::new(name, MetricKind::Gauge))
  }

  /// The histogram `name` over `boundaries` (no tags).
  pub fn histogram(&self, name: impl Into<Arc<str>>, boundaries: Boundaries) -> Histogram {
    Histogram::from_key(
      self,
      MetricKey::new(name, MetricKind::Histogram(boundaries)),
    )
  }

  /// The summary `name` with `config` (no tags).
  pub fn summary(&self, name: impl Into<Arc<str>>, config: SummaryConfig) -> Summary {
    Summary::from_key(self, MetricKey::new(name, MetricKind::Summary(config)))
  }

  /// The frequency `name` (no tags).
  pub fn frequency(&self, name: impl Into<Arc<str>>) -> Frequency {
    Frequency::from_key(self, MetricKey::new(name, MetricKind::Frequency))
  }

  /// The number of keys with state.
  pub fn len(&self) -> usize {
    self
      .inner
      .cells
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .len()
  }

  /// `true` when no key has state.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// The reading of `key`, if it has state.
  pub fn get(&self, key: &MetricKey) -> Option<MetricState> {
    let cell = self
      .inner
      .cells
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .get(key)
      .cloned();
    cell.map(|c| c.snapshot(Instant::now()))
  }

  /// Every key (name, kind and tags) with its reading, summary windows aged to `now`; sorted by
  /// name, kind label and tags.
  pub fn snapshot_at(&self, now: Instant) -> Vec<MetricPair> {
    let cells: Vec<(MetricKey, MetricCell)> = self
      .inner
      .cells
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .iter()
      .map(|(k, c)| (k.clone(), c.clone()))
      .collect();
    let mut out: Vec<MetricPair> = cells
      .into_iter()
      .map(|(key, cell)| MetricPair {
        state: cell.snapshot(now),
        key,
      })
      .collect();
    out.sort_by(|a, b| {
      (a.key.name(), a.key.kind().label(), a.key.tags()).cmp(&(
        b.key.name(),
        b.key.kind().label(),
        b.key.tags(),
      ))
    });
    out
  }

  /// [`snapshot_at`](Self::snapshot_at) the wall clock (Effect.ts `MetricRegistry.snapshot`).
  pub fn snapshot(&self) -> Vec<MetricPair> {
    self.snapshot_at(Instant::now())
  }

  /// Adds a push listener. Keys registered earlier are replayed to its
  /// [`on_register`](MetricListener::on_register) first.
  pub fn add_listener(&self, listener: Arc<dyn MetricListener>) {
    let keys: Vec<MetricKey> = self
      .inner
      .cells
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .keys()
      .cloned()
      .collect();
    for k in &keys {
      listener.on_register(k);
    }
    let mut l = self
      .inner
      .listeners
      .write()
      .unwrap_or_else(PoisonError::into_inner);
    let mut v: Vec<Arc<dyn MetricListener>> = l.iter().cloned().collect();
    v.push(listener);
    *l = v.into();
    self.inner.has_listeners.store(true, Ordering::Release);
  }

  /// Removes every listener.
  pub fn clear_listeners(&self) {
    let mut l = self
      .inner
      .listeners
      .write()
      .unwrap_or_else(PoisonError::into_inner);
    *l = Arc::from(Vec::new());
    self.inner.has_listeners.store(false, Ordering::Release);
  }

  fn listeners(&self) -> Option<Listeners> {
    if !self.inner.has_listeners.load(Ordering::Acquire) {
      return None;
    }
    Some(Arc::clone(
      &self
        .inner
        .listeners
        .read()
        .unwrap_or_else(PoisonError::into_inner),
    ))
  }

  /// The state for `key`, created (and announced to listeners) on first use.
  pub(crate) fn cell(&self, key: &MetricKey) -> MetricCell {
    if let Some(c) = self
      .inner
      .cells
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .get(key)
    {
      return c.clone();
    }
    let (cell, created) = {
      let mut cells = self
        .inner
        .cells
        .write()
        .unwrap_or_else(PoisonError::into_inner);
      match cells.get(key) {
        Some(c) => (c.clone(), false),
        None => {
          let c = MetricCell::for_kind(key.kind());
          cells.insert(key.clone(), c.clone());
          (c, true)
        }
      }
    };
    if created && let Some(ls) = self.listeners() {
      for l in ls.iter() {
        l.on_register(key);
      }
    }
    cell
  }

  /// Hands `update` of `key` to every listener.
  pub(crate) fn notify(&self, key: &MetricKey, update: MetricUpdate<'_>) {
    if let Some(ls) = self.listeners() {
      for l in ls.iter() {
        l.on_update(key, update);
      }
    }
  }
}
