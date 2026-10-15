//! Observability: atomic metrics (counter, gauge, histogram, bounded summary, frequency), the
//! [`MetricRegistry`] with tagged snapshots, and the span / [`FiberRef`] hooks the otel crate
//! builds on. Filled by `p-metrics`.
//!
//! # Metrics
//!
//! Effect.ts `Metric` semantics: see [`metric`](self::metric) for the kinds and the
//! [`Metric`] trait (`update`, `track_all`, `track`, `track_success_with`, `track_error_with`,
//! `track_exit_with`, `track_duration_with`), and [`registry`](self::registry) for the store.
//!
//! ```
//! use id_effect::Exit;
//! use id_effect::next::{Effect, Metric, MetricRegistry, MetricState, succeed};
//!
//! let registry = MetricRegistry::new();
//! let requests = registry.counter("requests").tagged("route", "/health");
//! let eff: Effect<u32, ()> = requests.track_all(1, succeed(7)).tag_metrics("env", "test");
//! assert!(matches!(eff.block_on(), Exit::Success(7)));
//!
//! let snap = registry.snapshot();
//! let pair = snap.iter().find(|p| p.key.name() == "requests" && p.key.tags().len() == 2);
//! assert_eq!(pair.unwrap().state, MetricState::Counter { count: 1 });
//! ```
//!
//! # Spans
//!
//! [`Effect::with_span`] runs an effect inside a [`Span`]: the span is the value of the
//! [`Span::fiber_ref`] fiber ref for the duration (so children forked inside inherit it as their
//! parent span), its parent is the span that was current, and the fiber's [`SpanHook`] (if one
//! was installed with [`Effect::with_span_hook`]) is told when it starts and when it ends with
//! the [`ExitKind`]. Times come from the fiber's executor clock. With no hook installed a span
//! costs one id, one `Arc` and the fiber ref swap.
//!
//! # Bridge surface for `m-otel` (`crates/id_effect_opentelemetry`)
//!
//! - **Metrics, pull** — [`MetricRegistry::snapshot`] (or `snapshot_at`) on
//!   [`MetricRegistry::global`] yields every [`MetricPair`]: name, [`MetricKind`] (histogram
//!   boundaries / summary config), sorted tags and [`MetricState`]. Counters and gauges map to
//!   otel observable instruments read in their callback.
//! - **Metrics, push** — a [`MetricListener`] added with [`MetricRegistry::add_listener`] gets
//!   `on_register(key)` for each series (existing ones replayed) and `on_update(key, update)` on
//!   each record ([`MetricUpdate`]); histograms map to otel synchronous histograms this way.
//! - **Metric tags from the fiber** — [`MetricRegistry::fiber_tags`] is the `FiberRef` behind
//!   [`Effect::tag_metrics`]; a bridge can read it with [`FiberRef::get_in`] from a
//!   [`FiberContext`](crate::next::FiberContext).
//! - **Spans** — implement [`SpanHook`] and install it around the root effect with
//!   [`Effect::with_span_hook`] (a `FiberRef`, inherited by every forked child; no global
//!   state, no thread-locals). In `on_start` create the otel span with the parent taken from
//!   [`Span::parent`]'s extension and stash it with [`Span::set_extension`]; in `on_end` read it
//!   back with [`Span::extension`] and end it with the outcome. [`Span::fiber_ref`] /
//!   [`Span::current_in`] give the current span to other integrations (log correlation).
//!
//! - **In-process buffer** — [`tracing`](self::tracing): the 0.6 `install_tracing_layer` /
//!   `snapshot_tracing` event buffer, fed by the [`TracingHook`] span hook.
//!
//! Nothing here uses thread-local storage: all per-fiber state lives in `FiberRef` slots of the
//! fiber's context (design A §1.3).

pub mod log_level;
pub mod metric;
pub mod registry;
pub mod tracing;

pub use log_level::LogLevel;
pub use metric::*;
pub use registry::*;
pub use tracing::*;

use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use crate::failure::Cause;
use crate::next::kernel::{Effect, ExitKind, Instr, MaskOp, erase_exit, sync, unerase_exit};
use crate::next::runtime::{FiberContext, FiberRef};

/// Runs `eff`, then `f(&outcome, fiber)` however it ends (success, failure, defect or
/// interrupt), then re-raises the outcome. `eff` keeps the caller's interruptibility; `f` runs
/// masked, as a synchronous step (a panic in `f` replaces the outcome with `Die`).
pub(crate) fn observe_exit_with_fiber<A, E, R>(
  eff: Effect<A, E, R>,
  f: impl FnOnce(&Result<A, Cause<E>>, &Arc<FiberContext>) + Send + 'static,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
    let m = fiber.mask();
    let fiber = Arc::clone(fiber);
    Instr::Mask(
      Box::new(Instr::Fold(
        Box::new(Instr::Mask(Box::new(eff.into_instr()), MaskOp::Set(m))),
        Box::new(move |exit| {
          let exit = unerase_exit::<A, E>(exit);
          Instr::Sync(Box::new(move |_| {
            f(&exit, &fiber);
            Instr::Pure(erase_exit(exit))
          }))
        }),
      )),
      MaskOp::Inc,
    )
  })))
}

// ── spans ───────────────────────────────────────────────────────────────────────────────────

/// A span id, unique in the process (never 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpanId(pub u64);

/// A trace id, shared by a root span and all its descendants (never 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TraceId(pub u128);

static NEXT_SPAN: AtomicU64 = AtomicU64::new(1);

fn splitmix(mut z: u64) -> u64 {
  z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
  z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
  z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
  z ^ (z >> 31)
}

struct SpanInner {
  id: SpanId,
  trace_id: TraceId,
  parent: Option<Span>,
  name: Arc<str>,
  attributes: Vec<(String, String)>,
  start: Instant,
  extension: OnceLock<Box<dyn Any + Send + Sync>>,
}

/// A running (or finished) span: name, attributes, ids, parent and start time, plus one
/// extension slot for the installed [`SpanHook`]. Cloning shares the span.
#[derive(Clone)]
pub struct Span {
  inner: Arc<SpanInner>,
}

impl std::fmt::Debug for Span {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Span")
      .field("id", &self.inner.id)
      .field("trace_id", &self.inner.trace_id)
      .field("parent", &self.inner.parent.as_ref().map(Span::id))
      .field("name", &self.inner.name)
      .field("attributes", &self.inner.attributes)
      .finish()
  }
}

impl Span {
  fn new(
    name: Arc<str>,
    attributes: Vec<(String, String)>,
    parent: Option<Span>,
    start: Instant,
  ) -> Self {
    let n = NEXT_SPAN.fetch_add(1, Ordering::Relaxed);
    let trace_id = match &parent {
      Some(p) => p.trace_id(),
      None => TraceId((u128::from(splitmix(n)) << 64 | u128::from(splitmix(!n))) | 1),
    };
    Self {
      inner: Arc::new(SpanInner {
        id: SpanId(n),
        trace_id,
        parent,
        name,
        attributes,
        start,
        extension: OnceLock::new(),
      }),
    }
  }

  /// The fiber ref holding the current span (`None` outside any span).
  pub fn fiber_ref() -> &'static FiberRef<Option<Span>> {
    static CURRENT: OnceLock<FiberRef<Option<Span>>> = OnceLock::new();
    CURRENT.get_or_init(|| FiberRef::new(None))
  }

  /// The fiber ref holding the installed hook (see [`Effect::with_span_hook`]).
  pub fn hook_ref() -> &'static FiberRef<Option<Arc<dyn SpanHook>>> {
    static HOOK: OnceLock<FiberRef<Option<Arc<dyn SpanHook>>>> = OnceLock::new();
    HOOK.get_or_init(|| FiberRef::new(None))
  }

  /// The running fiber's current span.
  pub fn current<E: Send + 'static, R: Send + Sync + 'static>() -> Effect<Option<Span>, E, R> {
    Self::fiber_ref().get()
  }

  /// `fiber`'s current span, outside an effect.
  pub fn current_in(fiber: &FiberContext) -> Option<Span> {
    Self::fiber_ref().get_in(fiber)
  }

  /// The span id.
  pub fn id(&self) -> SpanId {
    self.inner.id
  }

  /// The trace id (the root span's).
  pub fn trace_id(&self) -> TraceId {
    self.inner.trace_id
  }

  /// The enclosing span when this one started.
  pub fn parent(&self) -> Option<&Span> {
    self.inner.parent.as_ref()
  }

  /// The span name.
  pub fn name(&self) -> &str {
    &self.inner.name
  }

  /// The attributes given at start.
  pub fn attributes(&self) -> &[(String, String)] {
    &self.inner.attributes
  }

  /// The executor-clock instant the span started.
  pub fn start(&self) -> Instant {
    self.inner.start
  }

  /// The hook's extension value, if it set one of type `T`.
  pub fn extension<T: Any>(&self) -> Option<&T> {
    self
      .inner
      .extension
      .get()
      .and_then(|b| b.downcast_ref::<T>())
  }

  /// Stores the hook's extension value (once); `false` if one was already set.
  pub fn set_extension<T: Any + Send + Sync>(&self, value: T) -> bool {
    self.inner.extension.set(Box::new(value)).is_ok()
  }
}

/// Span lifecycle hook (the otel span bridge). Both methods run as synchronous steps of the
/// fiber that runs the span; a panic becomes that effect's `Die`.
pub trait SpanHook: Send + Sync + 'static {
  /// `span` started (its parent and fiber ref are set).
  fn on_start(&self, _span: &Span) {}

  /// `span` ended at `end` (executor clock) with outcome `exit`.
  fn on_end(&self, _span: &Span, _end: Instant, _exit: ExitKind) {}
}

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Runs this effect inside a new span named `name` (Effect.ts `Effect.withSpan`).
  pub fn with_span(self, name: impl Into<Arc<str>>) -> Self {
    self.with_span_attrs(name, std::iter::empty::<(String, String)>())
  }

  /// [`with_span`](Self::with_span) with attributes.
  pub fn with_span_attrs<K: Into<String>, V: Into<String>>(
    self,
    name: impl Into<Arc<str>>,
    attributes: impl IntoIterator<Item = (K, V)>,
  ) -> Self {
    let name = name.into();
    let attributes: Vec<(String, String)> = attributes
      .into_iter()
      .map(|(k, v)| (k.into(), v.into()))
      .collect();
    Self::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
      let parent = Span::current_in(fiber);
      let hook = Span::hook_ref().get_in(fiber);
      let span = Span::new(name, attributes, parent, fiber.executor().now());
      let start = {
        let (hook, span) = (hook.clone(), span.clone());
        sync::<(), E, R>(move || {
          if let Some(h) = &hook {
            h.on_start(&span);
          }
        })
      };
      let body = start.then(Span::fiber_ref().locally(Some(span.clone()), self));
      observe_exit_with_fiber(body, move |exit, fiber| {
        if let Some(h) = &hook {
          h.on_end(&span, fiber.executor().now(), ExitKind::of_result(exit));
        }
      })
      .into_instr()
    })))
  }

  /// Runs this effect with `hook` as the fiber's span hook (inherited by forked children).
  pub fn with_span_hook(self, hook: Arc<dyn SpanHook>) -> Self {
    Span::hook_ref().locally(Some(hook), self)
  }

  /// Runs this effect with metric tag `key = value` added to the fiber's metric tags (Effect.ts
  /// `Effect.tagMetrics`): every effect-level metric update inside records under the tagged key.
  pub fn tag_metrics(self, key: impl Into<String>, value: impl Into<String>) -> Self {
    let tag = [(key.into(), value.into())];
    Self::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
      let tags = MetricRegistry::fiber_tags();
      let merged = merge_tags(&tags.get_in(fiber), &tag);
      tags.locally(merged, self).into_instr()
    })))
  }
}
