//! `p-layer-core` acceptance tests for `id_effect::Layer`: `succeed`, `effect`, `merge`,
//! `provide_to`; providing a layer yields a runnable empty-env effect; a dependency shared
//! inside a merge is built once (memoisation by reference within one build, Effect.ts `Layer`
//! semantics). The `k-layer-base` cases stay in `tests/layer_base.rs`.
//!
//! Effects are driven with `pollster::block_on` over a test-local `Executor` stub.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use id_effect::{
  BoxFuture, Effect, Executor, FiberContext, HCons, HNil, Layer, LocalBoxFuture, NotLocal, fail,
  hlist, need, sleep, sync,
};
use id_effect::{Cause, Exit};

struct StubExec;

impl Executor for StubExec {
  fn spawn(&self, _: BoxFuture<'static, ()>) {
    unreachable!("layer tests spawn no tasks")
  }
  fn spawn_local(&self, _: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal> {
    Err(NotLocal)
  }
  fn spawn_blocking(&self, _: Box<dyn FnOnce() + Send>) {
    unreachable!("layer tests spawn no blocking work")
  }
  fn sleep(&self, _: Duration) -> BoxFuture<'static, ()> {
    Box::pin(async {})
  }
  fn now(&self) -> Instant {
    Instant::now()
  }
}

fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
  pollster::block_on(eff.run_on(FiberContext::new_root(Arc::new(StubExec))))
}

fn ok<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(eff: Effect<A, E>) -> A {
  match run(eff) {
    Exit::Success(a) => a,
    Exit::Failure(c) => panic!("expected success, got {c:?}"),
  }
}

#[derive(Debug, PartialEq)]
enum AppError {
  Boom,
}

// ── fixtures ────────────────────────────────────────────────────────────────────────────────

struct Config(&'static str);
struct Db {
  url: &'static str,
  cfg: Arc<Config>,
}
struct Mailer {
  cfg: Arc<Config>,
}
struct Cache {
  cfg: Arc<Config>,
}
struct Clock;

type Cfg = HCons<Config, HNil>;

/// A closed `Config` layer that counts its constructions (`effect`: rebuildable).
fn counted_config(url: &'static str, built: &Arc<AtomicUsize>) -> Layer<Cfg, AppError> {
  let built = Arc::clone(built);
  Layer::effect(move || {
    let built = Arc::clone(&built);
    sync(move || {
      built.fetch_add(1, Ordering::SeqCst);
      hlist![Config(url)]
    })
  })
}

fn db() -> Layer<HCons<Db, HNil>, AppError, Cfg> {
  Layer::effect(|| {
    need::<Config>().map(|cfg| {
      hlist![Db {
        url: cfg.0,
        cfg: Arc::clone(&cfg),
      }]
    })
  })
}

fn mailer() -> Layer<HCons<Mailer, HNil>, AppError, Cfg> {
  Layer::effect(|| need::<Config>().map(|cfg| hlist![Mailer { cfg }]))
}

/// Construction goes through a real async step (`sleep`), not only the synchronous fast path.
fn cache() -> Layer<HCons<Cache, HNil>, AppError, Cfg> {
  Layer::effect(|| {
    sleep(Duration::from_millis(1)).flat_map(|()| need::<Config>().map(|cfg| hlist![Cache { cfg }]))
  })
}

fn url_of_db() -> Effect<&'static str, AppError, HCons<Db, HNil>> {
  need::<Db>().map(|db| db.url)
}

// ── AC1: succeed, effect, merge, provide_to ────────────────────────────────────────────────

#[test]
fn succeed_hands_out_the_same_arcs_on_every_build() {
  let layer = Layer::<_, AppError>::succeed(hlist![Config("mem://s"), Clock]);
  let (a, b) = (ok(layer.build()), ok(layer.build()));
  assert_eq!(a.0.0, "mem://s");
  assert!(Arc::ptr_eq(&a.0, &b.0));
  assert!(Arc::ptr_eq(&a.1.0, &b.1.0));
}

#[test]
fn effect_is_lazy_and_rebuilds_on_every_build() {
  let built = Arc::new(AtomicUsize::new(0));
  let cfg = counted_config("mem://e", &built);
  assert_eq!(built.load(Ordering::SeqCst), 0, "layers are lazy");
  let first = ok(cfg.build());
  assert_eq!(built.load(Ordering::SeqCst), 1);
  let second = ok(cfg.build());
  assert_eq!(
    built.load(Ordering::SeqCst),
    2,
    "each build has its own memo table"
  );
  assert!(!Arc::ptr_eq(&first.0, &second.0));
}

#[test]
fn merge_concatenates_outputs_and_a_failure_fails_the_build() {
  let both = db().merge(mailer());
  let env = ok(both.build().provide_env(hlist![Config("mem://m")]));
  assert_eq!(env.0.url, "mem://m");
  assert!(Arc::ptr_eq(&env.0.cfg, &env.1.0.cfg));

  let bad: Layer<HCons<Clock, HNil>, AppError> = Layer::effect(|| fail(AppError::Boom));
  match run(Layer::succeed(hlist![Config("x")]).merge(bad).build()) {
    Exit::Failure(Cause::Fail(e)) => assert_eq!(e, AppError::Boom),
    _ => panic!("expected Fail(Boom)"),
  }
}

#[test]
fn provide_to_feeds_the_base_and_keeps_only_the_upper_output() {
  let built = Arc::new(AtomicUsize::new(0));
  let wired: Layer<HCons<Db, HNil>, AppError> = counted_config("mem://p", &built).provide_to(db());
  let env = ok(wired.build());
  assert_eq!((env.0.url, env.0.cfg.0), ("mem://p", "mem://p"));
  assert_eq!(built.load(Ordering::SeqCst), 1);

  // The base may produce extras, in any order; the result still needs only the base's input.
  let closed: Layer<HCons<Clock, Cfg>, AppError> = Layer::succeed(hlist![Clock, Config("mem://x")]);
  assert_eq!(ok(closed.provide_to(db()).build()).0.url, "mem://x");
  let open_base: Layer<HCons<Clock, Cfg>, AppError, HCons<Clock, HNil>> =
    Layer::effect(|| need::<Clock>().map(|clock| HCons(clock, hlist![Config("mem://y")])));
  let open: Layer<HCons<Db, HNil>, AppError, HCons<Clock, HNil>> = open_base.provide_to(db());
  assert_eq!(ok(open.build().provide_env(hlist![Clock])).0.url, "mem://y");
}

#[test]
fn provide_to_failure_in_the_base_skips_the_upper_layer() {
  let upper_built = Arc::new(AtomicUsize::new(0));
  let seen = Arc::clone(&upper_built);
  let upper: Layer<HCons<Db, HNil>, AppError, Cfg> = Layer::effect(move || {
    let seen = Arc::clone(&seen);
    need::<Config>().map(move |cfg| {
      seen.fetch_add(1, Ordering::SeqCst);
      hlist![Db {
        url: cfg.0,
        cfg: Arc::clone(&cfg),
      }]
    })
  });
  let base: Layer<Cfg, AppError> = Layer::effect(|| fail(AppError::Boom));
  match run(base.provide_to(upper).build()) {
    Exit::Failure(Cause::Fail(e)) => assert_eq!(e, AppError::Boom),
    _ => panic!("expected Fail(Boom)"),
  }
  assert_eq!(upper_built.load(Ordering::SeqCst), 0);
}

// ── AC2: providing a layer yields a runnable empty-env effect ──────────────────────────────

#[test]
fn providing_a_closed_layer_yields_a_runnable_empty_env_effect() {
  fn report() -> Effect<String, AppError, HCons<Mailer, HCons<Db, HNil>>> {
    need::<Mailer>().flat_map(|m| need::<Db>().map(move |db| format!("{}|{}", m.cfg.0, db.url)))
  }
  let app = db()
    .merge(mailer())
    .provide_merge(Layer::succeed(hlist![Config("mem://run")]));
  let runnable: Effect<String, AppError> = report().provide_layer(app);
  assert_eq!(ok(runnable), "mem://run|mem://run");

  let built = Arc::new(AtomicUsize::new(0));
  let runnable: Effect<&'static str, AppError, HNil> =
    url_of_db().provide_layer(counted_config("mem://r", &built).provide_to(db()));
  assert_eq!(built.load(Ordering::SeqCst), 0, "providing is lazy too");
  assert_eq!(ok(runnable), "mem://r");
  assert_eq!(built.load(Ordering::SeqCst), 1);
}

#[test]
fn a_failing_provided_layer_fails_the_effect_without_running_it() {
  let ran = Arc::new(AtomicUsize::new(0));
  let seen = Arc::clone(&ran);
  let program = url_of_db().map(move |u| {
    seen.fetch_add(1, Ordering::SeqCst);
    u
  });
  let base: Layer<Cfg, AppError> = Layer::effect(|| fail(AppError::Boom));
  match run(program.provide_layer(base.provide_to(db()))) {
    Exit::Failure(Cause::Fail(e)) => assert_eq!(e, AppError::Boom),
    _ => panic!("expected Fail(Boom)"),
  }
  assert_eq!(ran.load(Ordering::SeqCst), 0);
}

// ── AC3: a dependency shared inside a merge is built once ──────────────────────────────────

#[test]
fn a_dependency_shared_inside_a_merge_is_built_once() {
  let built = Arc::new(AtomicUsize::new(0));
  let cfg = counted_config("mem://shared", &built);
  let app = cfg
    .clone()
    .provide_to(db())
    .merge(cfg.clone().provide_to(mailer()))
    .merge(cfg.provide_to(cache()));
  let env = ok(app.build());
  assert_eq!(
    built.load(Ordering::SeqCst),
    1,
    "one construction for three users"
  );
  let (db, mailer, cache) = (&env.0, &env.1.0, &env.1.1.0);
  assert!(Arc::ptr_eq(&db.cfg, &mailer.cfg));
  assert!(Arc::ptr_eq(&db.cfg, &cache.cfg));
}

#[test]
fn a_shared_dependency_is_built_once_across_strata() {
  // `cfg` feeds the merged upper stratum and, separately, a sibling layer.
  let built = Arc::new(AtomicUsize::new(0));
  let cfg = counted_config("mem://strata", &built);
  let app = db()
    .merge(mailer())
    .provide_merge(cfg.clone())
    .merge(cfg.provide_to(cache()));
  let env = ok(app.build());
  assert_eq!(built.load(Ordering::SeqCst), 1);
  let (config, db, cache) = (&env.0, &env.1.0, &env.1.1.1.0);
  assert!(Arc::ptr_eq(config, &db.cfg));
  assert!(Arc::ptr_eq(config, &cache.cfg));
}

#[test]
fn a_shared_composite_layer_is_built_once() {
  // Memoisation is per node at every level: a shared `provide_to` result is reused whole.
  let built = Arc::new(AtomicUsize::new(0));
  let db_built = Arc::new(AtomicUsize::new(0));
  let seen = Arc::clone(&db_built);
  let counted_db: Layer<HCons<Db, HNil>, AppError, Cfg> = Layer::effect(move || {
    let seen = Arc::clone(&seen);
    need::<Config>().map(move |cfg| {
      seen.fetch_add(1, Ordering::SeqCst);
      hlist![Db {
        url: cfg.0,
        cfg: Arc::clone(&cfg),
      }]
    })
  });
  let live_db = counted_config("mem://c", &built).provide_to(counted_db);
  struct Audit;
  let clock: Layer<HCons<Clock, HNil>, AppError, HCons<Db, HNil>> =
    Layer::effect(|| need::<Db>().map(|_| hlist![Clock]));
  let audit: Layer<HCons<Audit, HNil>, AppError, HCons<Db, HNil>> =
    Layer::effect(|| need::<Db>().map(|_| hlist![Audit]));
  let app = live_db
    .clone()
    .provide_to(clock)
    .merge(live_db.provide_to(audit));
  ok(app.build());
  assert_eq!(built.load(Ordering::SeqCst), 1);
  assert_eq!(db_built.load(Ordering::SeqCst), 1);
}

#[test]
fn distinct_layer_values_are_built_separately() {
  // Two calls of a layer-returning function are two layers (Effect.ts: two `Layer.effect`s).
  let built = Arc::new(AtomicUsize::new(0));
  let app = counted_config("a", &built)
    .provide_to(db())
    .merge(counted_config("b", &built).provide_to(mailer()));
  let env = ok(app.build());
  assert_eq!(built.load(Ordering::SeqCst), 2);
  assert_eq!((env.0.url, env.1.0.cfg.0), ("a", "b"));
}

#[test]
fn each_provide_layer_run_builds_under_its_own_memo_table() {
  let built = Arc::new(AtomicUsize::new(0));
  let live = counted_config("mem://twice", &built).provide_to(db());
  assert_eq!(ok(url_of_db().provide_layer(live.clone())), "mem://twice");
  assert_eq!(ok(url_of_db().provide_layer(live)), "mem://twice");
  assert_eq!(built.load(Ordering::SeqCst), 2);
}

// ── from_effect: one-shot effects under memoisation ────────────────────────────────────────

#[test]
fn from_effect_is_shared_within_a_build_and_dies_on_a_second_build() {
  let built = Arc::new(AtomicUsize::new(0));
  let seen = Arc::clone(&built);
  let once: Layer<Cfg, AppError> = Layer::from_effect(sync(move || {
    seen.fetch_add(1, Ordering::SeqCst);
    hlist![Config("mem://once")]
  }));
  let app = once
    .clone()
    .provide_to(db())
    .merge(once.clone().provide_to(mailer()));
  let env = ok(app.build());
  assert_eq!(built.load(Ordering::SeqCst), 1);
  assert!(Arc::ptr_eq(&env.0.cfg, &env.1.0.cfg));

  match run(once.build()) {
    Exit::Failure(Cause::Die(d)) => {
      let msg = format!("{d:?}");
      assert!(msg.contains("already built once"), "{msg}");
    }
    _ => panic!("expected a Die on the second build of a from_effect layer"),
  }
}
