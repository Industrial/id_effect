//! `p-metrics` acceptance tests for `id_effect`: atomic counter / gauge / histogram /
//! frequency state, the bounded summary window, `MetricRegistry::snapshot` with tags, the
//! `track*` combinators, fiber metric tags, the push listener, and the span / `FiberRef` hooks.

use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::{
  Boundaries, Effect, Executor, ExitKind, Metric, MetricKey, MetricKind, MetricListener,
  MetricRegistry, MetricState, MetricUpdate, Span, SpanHook, SummaryConfig, TestExecutor, die,
  fail, sleep, succeed,
};
use id_effect::{Cause, Exit};

const SOURCES: [&str; 3] = [
  include_str!("../src/observability/metric.rs"),
  include_str!("../src/observability/registry.rs"),
  include_str!("../src/observability/mod.rs"),
];

fn ms(n: u64) -> Duration {
  Duration::from_millis(n)
}

fn tags(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
  pairs
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect()
}

/// Polls `fut` once; it must be ready.
fn take_ready<F: Future>(fut: F) -> F::Output {
  let mut fut = pin!(fut);
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("root fiber is not done"),
  }
}

/// Runs `eff` on a fresh `TestExecutor`, advancing the virtual clock timer by timer.
fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
  let exec = TestExecutor::new();
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  while !h.is_done() {
    let deadline = exec
      .next_timer_deadline()
      .expect("root fiber stalled with no timer pending");
    exec.advance(deadline.saturating_duration_since(exec.now()));
  }
  take_ready(h.join_future())
}

// ── AC: atomics, no Mutex<u64|i64>, no thread_local! ─────────────────────────────────────────

#[test]
fn state_is_atomic_and_never_a_mutex_over_an_integer() {
  for src in SOURCES {
    for banned in [
      "Mutex<u64>",
      "Mutex<i64>",
      "RwLock<u64>",
      "RwLock<i64>",
      "thread_local!",
    ] {
      assert!(
        !src.contains(banned),
        "observability source contains `{banned}`"
      );
    }
  }
  assert!(SOURCES[0].contains("AtomicU64") && SOURCES[0].contains("AtomicI64"));
}

// ── AC: 1e6 histogram samples, len == boundaries + 1 ─────────────────────────────────────────

#[test]
fn histogram_after_a_million_samples_has_boundaries_plus_one_buckets() {
  let registry = MetricRegistry::new();
  let bounds = Boundaries::linear(10.0, 10.0, 10);
  assert_eq!(bounds.len(), 10);
  let h = registry.histogram("latency", bounds.clone());

  // 4 threads x 250_000 samples, concurrently, through the shared atomic buckets.
  std::thread::scope(|s| {
    for t in 0..4u64 {
      let h = h.clone();
      s.spawn(move || {
        for i in 0..250_000u64 {
          h.update_now(((t * 250_000 + i) % 200) as f64);
        }
      });
    }
  });

  let snap = h.snapshot();
  assert_eq!(snap.buckets.len(), bounds.len() + 1);
  assert_eq!(snap.count, 1_000_000);
  assert_eq!(snap.buckets.iter().map(|(_, c)| c).sum::<u64>(), 1_000_000);
  assert_eq!(snap.buckets.last().map(|(b, _)| *b), Some(f64::INFINITY));
  // Values 0..=10 land in the first bucket (le 10): 11 of every 200.
  assert_eq!(snap.buckets[0], (10.0, 11 * 5_000));
  // Values 101..=199 overflow into +inf: 99 of every 200.
  assert_eq!(snap.buckets[10].1, 99 * 5_000);
  assert_eq!(snap.min, Some(0.0));
  assert_eq!(snap.max, Some(199.0));
  assert_eq!(snap.sum, (0..200).sum::<u64>() as f64 * 5_000.0);

  // The registry snapshot agrees.
  let pairs = registry.snapshot();
  let MetricState::Histogram(hs) = &pairs[0].state else {
    panic!("not a histogram: {pairs:?}");
  };
  assert_eq!(hs.buckets.len(), bounds.len() + 1);
}

#[test]
fn histogram_ignores_nan_and_puts_bound_values_in_their_bucket() {
  let h = MetricRegistry::new().histogram("h", Boundaries::new([1.0, 2.0, f64::NAN, 2.0]));
  assert_eq!(h.boundaries().as_slice(), &[1.0, 2.0]);
  for v in [1.0, 2.0, 2.5, f64::NAN] {
    h.update_now(v);
  }
  let s = h.snapshot();
  assert_eq!(s.buckets, vec![(1.0, 1), (2.0, 1), (f64::INFINITY, 1)]);
  assert_eq!(s.count, 3);
}

// ── AC: snapshot includes tags ───────────────────────────────────────────────────────────────

#[test]
fn registry_snapshot_includes_tags_and_keeps_series_apart() {
  let registry = MetricRegistry::new();
  let base = registry.counter("requests");
  let a = base.tagged("route", "/a").tagged("method", "GET");
  let b = base.tagged("route", "/b");
  base.update_now(1);
  a.update_now(2);
  a.update_now(3);
  b.update_now(4);
  // The same key again is the same series.
  registry
    .counter("requests")
    .tagged_all(&tags(&[("method", "GET"), ("route", "/a")]))
    .update_now(5);

  let snap = registry.snapshot();
  type Row = (String, Vec<(String, String)>, MetricState);
  let got: Vec<Row> = snap
    .into_iter()
    .map(|p| (p.key.name().to_owned(), p.key.tags().to_vec(), p.state))
    .collect();
  assert_eq!(
    got,
    vec![
      ("requests".into(), vec![], MetricState::Counter { count: 1 }),
      (
        "requests".into(),
        tags(&[("method", "GET"), ("route", "/a")]),
        MetricState::Counter { count: 10 }
      ),
      (
        "requests".into(),
        tags(&[("route", "/b")]),
        MetricState::Counter { count: 4 }
      ),
    ]
  );
  assert_eq!(
    a.tags(),
    tags(&[("method", "GET"), ("route", "/a")]).as_slice()
  );
  let key = MetricKey::new("requests", MetricKind::Counter).with_tags(&tags(&[("route", "/b")]));
  assert_eq!(registry.get(&key), Some(MetricState::Counter { count: 4 }));
}

#[test]
fn a_tag_with_the_same_key_replaces_the_old_value() {
  let c = MetricRegistry::new()
    .counter("c")
    .tagged("k", "1")
    .tagged("k", "2");
  assert_eq!(c.tags(), tags(&[("k", "2")]).as_slice());
}

// ── AC: the summary window is bounded ───────────────────────────────────────────────────────

#[test]
fn summary_window_is_bounded_by_size() {
  let s = MetricRegistry::new().summary(
    "s",
    SummaryConfig::new(Duration::from_secs(3600), 100, [0.5, 0.99, 1.0, 7.0]),
  );
  assert_eq!(s.config().quantiles(), &[0.5, 0.99, 1.0]);
  for i in 0..10_000 {
    s.update_now(f64::from(i));
  }
  let snap = s.snapshot();
  assert_eq!(snap.window_len, 100);
  assert_eq!(snap.count, 10_000);
  // The window holds the last 100 samples: 9900..=9999.
  assert_eq!(
    snap.quantiles,
    vec![
      (0.5, Some(9949.0)),
      (0.99, Some(9998.0)),
      (1.0, Some(9999.0))
    ]
  );
  assert_eq!(snap.min, Some(0.0));
  assert_eq!(snap.max, Some(9999.0));
}

#[test]
fn summary_window_is_bounded_by_age() {
  let s =
    MetricRegistry::new().summary("s", SummaryConfig::new(Duration::from_secs(1), 1000, [0.5]));
  let t0 = Instant::now();
  s.record(1.0, t0);
  s.record(2.0, t0 + ms(500));
  s.record(3.0, t0 + ms(1600));
  // At t0 + 1.6 s only the last sample is younger than 1 s.
  let snap = s.snapshot_at(t0 + ms(1600));
  assert_eq!(snap.window_len, 1);
  assert_eq!(snap.quantiles, vec![(0.5, Some(3.0))]);
  // Later still, the window empties.
  let snap = s.snapshot_at(t0 + Duration::from_secs(10));
  assert_eq!(snap.window_len, 0);
  assert_eq!(snap.quantiles, vec![(0.5, None)]);
  assert_eq!(snap.count, 3);
}

// ── effect combinators ──────────────────────────────────────────────────────────────────────

#[test]
fn counter_gauge_and_frequency_update_as_effects() {
  let registry = MetricRegistry::new();
  let c = registry.counter("c");
  let g = registry.gauge("g");
  let f = registry.frequency("f");
  let eff: Effect<(), ()> = c
    .increment()
    .then(c.increment_by(4))
    .then(g.set(10))
    .then(g.adjust(-3))
    .then(f.occur("a"))
    .then(f.occur("b"))
    .then(f.occur("a"));
  assert!(matches!(eff.block_on(), Exit::Success(())));
  assert_eq!(c.count(), 5);
  assert_eq!(g.value(), 7);
  assert_eq!(f.occurrences(), vec![("a".into(), 2), ("b".into(), 1)]);
  c.update_now(u64::MAX);
  assert_eq!(c.count(), u64::MAX, "counters saturate");
}

#[test]
fn track_all_records_however_the_effect_ends() {
  let c = MetricRegistry::new().counter("runs");
  assert!(matches!(
    c.track_all(1, succeed::<u8, (), _>(1)).block_on(),
    Exit::Success(1)
  ));
  assert!(matches!(
    c.track_all(1, fail::<u8, &str, _>("no")).block_on(),
    Exit::Failure(Cause::Fail("no"))
  ));
  assert!(matches!(
    c.track_all(1, die::<u8, (), _>("boom")).block_on(),
    Exit::Failure(Cause::Die(_))
  ));
  assert_eq!(c.count(), 3);
}

#[test]
fn track_success_error_and_exit_pick_their_outcomes() {
  let registry = MetricRegistry::new();
  let ok = registry.histogram("ok", Boundaries::linear(1.0, 1.0, 5));
  let err = registry.frequency("err");
  let kinds = registry.frequency("kinds");
  let total = registry.counter("total");

  let run = |eff: Effect<u32, String>| {
    let eff = ok.track_success_with(eff, |v| f64::from(*v));
    let eff = err.track_error_with(eff, Clone::clone);
    let eff = kinds.track_exit_with(eff, |k| Some(format!("{k:?}")));
    let _ = eff.block_on();
  };
  run(succeed(3));
  run(fail("timeout".into()));
  run(fail("timeout".into()));
  run(die("boom"));
  assert_eq!(
    total
      .track(succeed::<u64, (), _>(9))
      .block_on()
      .into_result()
      .ok(),
    Some(9)
  );

  assert_eq!(ok.snapshot().count, 1);
  assert_eq!(ok.snapshot().sum, 3.0);
  assert_eq!(err.occurrences(), vec![("timeout".into(), 2)]);
  assert_eq!(
    kinds.occurrences(),
    vec![
      ("Die".into(), 1),
      ("Failure".into(), 2),
      ("Success".into(), 1)
    ]
  );
  assert_eq!(total.count(), 9);
}

#[test]
fn track_duration_uses_the_executor_clock() {
  let registry = MetricRegistry::new();
  let h = registry.histogram("d", Boundaries::new([0.01, 0.05, 0.1]));
  let s = registry.summary("ds", SummaryConfig::new(Duration::from_secs(60), 10, [0.5]));
  let eff: Effect<u8, ()> = h.track_duration(s.track_duration(sleep(ms(30)).map(|()| 7)));
  let started = Instant::now();
  assert!(matches!(run_virtual(eff), Exit::Success(7)));
  assert!(started.elapsed() < ms(30), "virtual time, not wall time");
  let snap = h.snapshot();
  assert_eq!(
    snap.buckets.iter().map(|(_, c)| *c).collect::<Vec<_>>(),
    vec![0, 1, 0, 0]
  );
  assert!((snap.sum - 0.03).abs() < 1e-9, "sum = {}", snap.sum);
  assert_eq!(s.snapshot().count, 1);
}

#[test]
fn an_interrupted_tracked_effect_still_records() {
  let kinds = MetricRegistry::new().frequency("kinds");
  let exec = TestExecutor::new();
  let eff: Effect<(), ()> =
    kinds.track_exit_with(sleep(Duration::from_secs(5)), |k| Some(format!("{k:?}")));
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  h.interrupt_now();
  exec.run_until_stalled();
  assert!(h.is_done());
  assert!(matches!(
    take_ready(h.join_future()),
    Exit::Failure(Cause::Interrupt(_))
  ));
  assert_eq!(kinds.occurrences(), vec![("Interrupt".into(), 1)]);
}

// ── fiber metric tags ───────────────────────────────────────────────────────────────────────

#[test]
fn tag_metrics_tags_every_update_in_the_fiber_and_its_children() {
  let registry = MetricRegistry::new();
  let c = registry.counter("hits");
  let child = c.increment::<(), _>().fork().widen().flat_map(|h| h.join());
  let eff: Effect<(), ()> = c
    .increment()
    .then(child)
    .tag_metrics("env", "prod")
    .tag_metrics("region", "eu")
    .then(c.increment());
  assert!(matches!(eff.block_on(), Exit::Success(())));

  assert_eq!(c.count(), 1, "outside the tagged region");
  let tagged = c.tagged_all(&tags(&[("env", "prod"), ("region", "eu")]));
  assert_eq!(tagged.count(), 2, "the fiber and its forked child");
  assert_eq!(registry.len(), 2);
}

// ── push listener ──────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Recorder {
  registered: Mutex<Vec<String>>,
  updates: Mutex<Vec<String>>,
}

impl MetricListener for Recorder {
  fn on_register(&self, key: &MetricKey) {
    self.registered.lock().unwrap().push(format!(
      "{}:{}:{:?}",
      key.kind().label(),
      key.name(),
      key.tags()
    ));
  }

  fn on_update(&self, key: &MetricKey, update: MetricUpdate<'_>) {
    self
      .updates
      .lock()
      .unwrap()
      .push(format!("{}={update:?}", key.name()));
  }
}

#[test]
fn listeners_see_registrations_and_updates() {
  let registry = MetricRegistry::new();
  let early = registry.counter("early");
  let rec = Arc::new(Recorder::default());
  registry.add_listener(rec.clone());
  let g = registry.gauge("g").tagged("k", "v");
  let h = registry.histogram("h", Boundaries::new([1.0]));
  let f = registry.frequency("f");
  early.update_now(2);
  g.update_now(5);
  g.adjust_now(1);
  h.update_now(0.5);
  f.update_now("x".into());

  assert_eq!(
    *rec.registered.lock().unwrap(),
    vec![
      "counter:early:[]".to_owned(),
      r#"gauge:g:[("k", "v")]"#.to_owned(),
      "histogram:h:[]".to_owned(),
      "frequency:f:[]".to_owned(),
    ]
  );
  assert_eq!(
    *rec.updates.lock().unwrap(),
    vec![
      "early=Counter(2)",
      "g=Gauge(5)",
      "g=Gauge(6)",
      "h=Histogram(0.5)",
      r#"f=Frequency("x")"#
    ]
  );
  registry.clear_listeners();
  early.update_now(1);
  assert_eq!(rec.updates.lock().unwrap().len(), 5);
}

// ── spans ──────────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct SpanLog(Mutex<Vec<String>>);

impl SpanHook for SpanLog {
  fn on_start(&self, span: &Span) {
    let parent = span.parent().map(|p| p.name().to_owned());
    assert!(span.set_extension(format!("ext-{}", span.name())));
    self
      .0
      .lock()
      .unwrap()
      .push(format!("start {} parent={parent:?}", span.name()));
  }

  fn on_end(&self, span: &Span, end: Instant, exit: ExitKind) {
    assert!(end >= span.start());
    let ext = span.extension::<String>().cloned();
    self
      .0
      .lock()
      .unwrap()
      .push(format!("end {} {exit:?} {ext:?}", span.name()));
  }
}

#[test]
fn spans_nest_through_the_fiber_ref_and_report_to_the_hook() {
  let log = Arc::new(SpanLog::default());
  let inner: Effect<Option<Span>, &str> = Span::current().with_span_attrs("inner", [("k", "v")]);
  let failing: Effect<(), &str> = fail::<(), &str, _>("bad").with_span("failing");
  let eff = inner
    .flat_map(|inner_span| {
      Span::current().map(move |outer_span| (inner_span.unwrap(), outer_span.unwrap()))
    })
    .flat_map(|pair| failing.catch(|_| succeed(())).map(move |()| pair))
    .with_span("outer")
    .with_span_hook(log.clone());

  let Exit::Success((inner, outer)) = eff.block_on() else {
    panic!("span effect failed");
  };
  assert_eq!(inner.name(), "inner");
  assert_eq!(inner.attributes(), tags(&[("k", "v")]).as_slice());
  assert_eq!(inner.parent().map(Span::id), Some(outer.id()));
  assert_eq!(inner.trace_id(), outer.trace_id());
  assert_ne!(inner.id(), outer.id());
  assert!(outer.parent().is_none());

  assert_eq!(
    *log.0.lock().unwrap(),
    vec![
      "start outer parent=None",
      r#"start inner parent=Some("outer")"#,
      r#"end inner Success Some("ext-inner")"#,
      r#"start failing parent=Some("outer")"#,
      r#"end failing Failure Some("ext-failing")"#,
      r#"end outer Success Some("ext-outer")"#,
    ]
  );
}

#[test]
fn the_span_is_restored_after_with_span_and_inherited_by_children() {
  let child_sees: Effect<Option<Span>, ()> = Span::current::<(), _>()
    .fork()
    .widen()
    .flat_map(|h| h.join());
  let eff = child_sees
    .with_span("parent")
    .flat_map(|in_child| Span::current().map(move |after| (in_child, after)));
  let Exit::Success((in_child, after)) = eff.block_on() else {
    panic!("failed");
  };
  assert_eq!(
    in_child.map(|s| s.name().to_owned()),
    Some("parent".to_owned())
  );
  assert!(after.is_none(), "with_span restores the previous span");
  // Root spans start distinct traces.
  let a = Span::current::<(), _>()
    .with_span("a")
    .block_on()
    .into_result()
    .ok()
    .flatten();
  let b = Span::current::<(), _>()
    .with_span("b")
    .block_on()
    .into_result()
    .ok()
    .flatten();
  assert_ne!(a.unwrap().trace_id(), b.unwrap().trace_id());
}
