//! `p-layer-scoped` acceptance tests for `id_effect::Layer`: scoped layers
//! (`Layer::scoped`, `Layer::acquire_release`) whose finalizers run in reverse order of
//! acquisition when the build's scope closes (after `provide_layer`'s effect, on interrupt, on a
//! failed build, on `Layer::launch`'s interrupt, or when the caller of `build_scoped` closes it);
//! `fresh()` builds again; finalizers are async effects.
//!
//! Every behavioural test runs on a multi-thread `TokioExecutor` **and** on the deterministic
//! `TestExecutor` through [`on_both`].

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::Never;
use id_effect::{Cause, Exit};
use id_effect::{
  Effect, Executor, ExitKind, HCons, HNil, Layer, Scope, TestExecutor, TokioExecutor, fail, hlist,
  need, sleep, succeed, suspend, sync, yield_fiber,
};

const HOUR: Duration = Duration::from_secs(3600);

// ── drivers ───────────────────────────────────────────────────────────────────────────────

fn tokio_rt() -> &'static tokio::runtime::Runtime {
  static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
  RT.get_or_init(|| {
    tokio::runtime::Builder::new_multi_thread()
      .worker_threads(4)
      .enable_time()
      .build()
      .expect("tokio runtime")
  })
}

/// Polls `fut` once; it must be ready (the test executor has already run everything).
fn take_ready<F: Future>(fut: F) -> F::Output {
  let mut fut = pin!(fut);
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("root fiber is not done"),
  }
}

#[derive(Clone, Copy, Debug)]
enum Driver {
  Tokio,
  Test,
}

impl Driver {
  /// Runs `eff` as a root fiber and returns its exit.
  fn run<A: Send + 'static, E: Send + 'static>(self, eff: Effect<A, E>) -> Exit<A, E> {
    match self {
      Driver::Tokio => {
        let rt = tokio_rt();
        let h = eff.fork_root(Arc::new(TokioExecutor::new(rt.handle().clone())));
        rt.block_on(async {
          tokio::time::timeout(Duration::from_secs(30), h.join_future())
            .await
            .expect("root fiber timed out on tokio")
        })
      }
      Driver::Test => {
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
    }
  }
}

/// Runs a test body once per executor.
fn on_both(t: impl Fn(Driver)) {
  t(Driver::Tokio);
  t(Driver::Test);
}

// ── helpers ───────────────────────────────────────────────────────────────────────────────

type Log = Arc<Mutex<Vec<String>>>;
type Fail = &'static str;

fn log() -> Log {
  Arc::new(Mutex::new(Vec::new()))
}

fn push<E: Send + 'static, R: Send + Sync + 'static>(
  l: &Log,
  s: impl Into<String>,
) -> Effect<(), E, R> {
  let (l, s) = (Arc::clone(l), s.into());
  sync(move || l.lock().expect("log").push(s))
}

fn entries(l: &Log) -> Vec<String> {
  l.lock().expect("log").clone()
}

fn flag() -> Arc<AtomicBool> {
  Arc::new(AtomicBool::new(false))
}

fn set<E: Send + 'static, R: Send + Sync + 'static>(f: &Arc<AtomicBool>) -> Effect<(), E, R> {
  let f = Arc::clone(f);
  sync(move || f.store(true, Ordering::SeqCst))
}

/// Yields until `f` is set (so another fiber has reached a known point).
fn started<E: Send + 'static>(f: &Arc<AtomicBool>) -> Effect<(), E> {
  fn go<E: Send + 'static>(f: Arc<AtomicBool>) -> Effect<(), E> {
    suspend(move || {
      if f.load(Ordering::SeqCst) {
        succeed(())
      } else {
        yield_fiber().flat_map(move |()| go(f))
      }
    })
  }
  go(Arc::clone(f))
}

struct Pool;
struct Repo;
struct Cache;

/// Needs `T` (typed `R`, so `provide_layer` can infer its projection).
fn uses<T: Send + Sync + 'static>() -> Effect<(), Fail, HCons<T, HNil>> {
  need::<T>().map(|_| ())
}

/// Needs `A` and `B`.
fn uses2<A: Send + Sync + 'static, B: Send + Sync + 'static>()
-> Effect<(), Fail, HCons<A, HCons<B, HNil>>> {
  need::<A>().flat_map(|_| need::<B>().map(|_| ()))
}

/// A closed resource layer: logs `acquire <name>` and, on release, `release <name> <kind>`.
fn resource<T: Send + Sync + 'static>(
  l: &Log,
  name: &'static str,
  make: fn() -> T,
) -> Layer<HCons<T, HNil>, Fail> {
  let (la, lr) = (Arc::clone(l), Arc::clone(l));
  Layer::acquire_release(
    move || push(&la, format!("acquire {name}")).map(move |()| hlist![make()]),
    move |_, kind| push(&lr, format!("release {name} {kind:?}")),
  )
}

/// `Repo` over `Pool`, a resource too.
fn repo(l: &Log) -> Layer<HCons<Repo, HNil>, Fail, HCons<Pool, HNil>> {
  let (la, lr) = (Arc::clone(l), Arc::clone(l));
  Layer::acquire_release(
    move || {
      let la = Arc::clone(&la);
      need::<Pool>().flat_map(move |_| push(&la, "acquire repo").map(|()| hlist![Repo]))
    },
    move |_, kind| push(&lr, format!("release repo {kind:?}")),
  )
}

/// pool → repo, merged with a cache: three resources acquired pool, repo, cache.
fn app(l: &Log) -> Layer<HCons<Repo, HCons<Cache, HNil>>, Fail> {
  resource(l, "pool", || Pool)
    .provide_to(repo(l))
    .merge(resource(l, "cache", || Cache))
}

fn use_repo(l: &Log) -> Effect<(), Fail, HCons<Repo, HCons<Cache, HNil>>> {
  let l = Arc::clone(l);
  need::<Repo>().flat_map(move |_| push(&l, "use"))
}

const ACQUIRED: [&str; 3] = ["acquire pool", "acquire repo", "acquire cache"];

fn released(kind: ExitKind) -> Vec<String> {
  ["cache", "repo", "pool"]
    .iter()
    .map(|n| format!("release {n} {kind:?}"))
    .collect()
}

fn concat(parts: &[&[String]]) -> Vec<String> {
  parts.iter().flat_map(|p| p.iter().cloned()).collect()
}

fn strings(s: &[&str]) -> Vec<String> {
  s.iter().map(|s| (*s).to_owned()).collect()
}

// ── AC1: finalizers run in reverse on close ─────────────────────────────────────────────────

#[test]
fn provide_layer_closes_the_scope_after_the_effect_in_reverse_order() {
  on_both(|d| {
    let l = log();
    let eff = use_repo(&l).provide_layer(app(&l));
    assert!(matches!(d.run(eff), Exit::Success(())), "{d:?}");
    assert_eq!(
      entries(&l),
      concat(&[
        &strings(&ACQUIRED),
        &strings(&["use"]),
        &released(ExitKind::Success)
      ]),
      "{d:?}"
    );
  });
}

#[test]
fn a_failing_effect_closes_the_scope_with_failure() {
  on_both(|d| {
    let l = log();
    let eff = use_repo(&l)
      .then(fail::<(), Fail, _>("boom"))
      .provide_layer(app(&l));
    assert!(
      matches!(d.run(eff), Exit::Failure(Cause::Fail("boom"))),
      "{d:?}"
    );
    assert_eq!(
      entries(&l),
      concat(&[
        &strings(&ACQUIRED),
        &strings(&["use"]),
        &released(ExitKind::Failure)
      ]),
      "{d:?}"
    );
  });
}

#[test]
fn a_failed_build_releases_what_was_acquired_in_reverse() {
  on_both(|d| {
    let l = log();
    let broken: Layer<HCons<Cache, HNil>, Fail> = Layer::effect(|| fail("cache down"));
    let layer = resource(&l, "pool", || Pool)
      .provide_to(repo(&l))
      .merge(broken);
    let eff = use_repo(&l).provide_layer(layer);
    assert!(
      matches!(d.run(eff), Exit::Failure(Cause::Fail("cache down"))),
      "{d:?}"
    );
    let rel = ["release repo Failure", "release pool Failure"];
    assert_eq!(
      entries(&l),
      strings(&["acquire pool", "acquire repo", rel[0], rel[1]]),
      "{d:?}"
    );

    // `build()` (no owner) also releases a failed build.
    let l = log();
    let broken: Layer<HCons<Cache, HNil>, Fail> = Layer::effect(|| fail("cache down"));
    let layer = resource(&l, "pool", || Pool).merge(broken);
    assert!(matches!(d.run(layer.build()), Exit::Failure(_)), "{d:?}");
    assert_eq!(
      entries(&l),
      strings(&["acquire pool", "release pool Failure"]),
      "{d:?}"
    );
  });
}

#[test]
fn a_successful_build_without_an_owner_runs_no_finalizers() {
  on_both(|d| {
    let l = log();
    assert!(matches!(d.run(app(&l).build()), Exit::Success(_)), "{d:?}");
    assert_eq!(entries(&l), strings(&ACQUIRED), "{d:?}");
  });
}

#[test]
fn build_scoped_releases_when_the_caller_closes_the_scope() {
  on_both(|d| {
    let l = log();
    let scope = Scope::new();
    let close = scope.clone();
    let l2 = Arc::clone(&l);
    let eff: Effect<(), Fail> = app(&l)
      .build_scoped(&scope)
      .flat_map(move |_| push(&l2, "built"))
      .then(close.close(ExitKind::Success).widen());
    assert!(matches!(d.run(eff), Exit::Success(())), "{d:?}");
    assert_eq!(
      entries(&l),
      concat(&[
        &strings(&ACQUIRED),
        &strings(&["built"]),
        &released(ExitKind::Success)
      ]),
      "{d:?}"
    );
    assert!(scope.is_closed());
  });
}

#[test]
fn scoped_layer_registers_finalizers_with_the_closing_kind() {
  on_both(|d| {
    let l = log();
    let lf = Arc::clone(&l);
    let layer: Layer<HCons<Pool, HNil>, Fail> = Layer::scoped(move |scope| {
      let (l1, l2) = (Arc::clone(&lf), Arc::clone(&lf));
      scope
        .add_finalizer(push(&l1, "first registered"))
        .then(scope.add_finalizer_exit(move |kind| push(&l2, format!("second {kind:?}"))))
        .map(|()| hlist![Pool])
    });
    let eff = uses::<Pool>()
      .then(fail::<(), _, _>("boom"))
      .provide_layer(layer);
    assert!(matches!(d.run(eff), Exit::Failure(_)), "{d:?}");
    assert_eq!(
      entries(&l),
      strings(&["second Failure", "first registered"]),
      "{d:?}"
    );
  });
}

// ── AC1: … and on interrupt ─────────────────────────────────────────────────────────────────

#[test]
fn interrupting_the_effect_releases_in_reverse_with_interrupt() {
  on_both(|d| {
    let (l, go) = (log(), flag());
    let body: Effect<(), Fail, HCons<Repo, HCons<Cache, HNil>>> = {
      let (l, go) = (Arc::clone(&l), Arc::clone(&go));
      need::<Repo>()
        .flat_map(move |_| push(&l, "use"))
        .then(set(&go))
        .then(sleep(HOUR))
    };
    let provided = body.provide_layer(app(&l));
    let eff = provided.fork().flat_map({
      let go = Arc::clone(&go);
      move |h| started(&go).then(h.interrupt())
    });
    let Exit::Success(ex) = d.run(eff) else {
      panic!("{d:?}: root failed")
    };
    assert!(
      matches!(ex, Exit::Failure(Cause::Interrupt(_))),
      "{d:?}: {ex:?}"
    );
    assert_eq!(
      entries(&l),
      concat(&[
        &strings(&ACQUIRED),
        &strings(&["use"]),
        &released(ExitKind::Interrupt)
      ]),
      "{d:?}"
    );
  });
}

#[test]
fn interrupting_the_build_releases_what_was_acquired() {
  on_both(|d| {
    let (l, go) = (log(), flag());
    let g = Arc::clone(&go);
    let stuck: Layer<HCons<Cache, HNil>, Fail> =
      Layer::effect(move || set(&g).then(sleep(HOUR)).map(|()| hlist![Cache]));
    let layer = resource(&l, "pool", || Pool)
      .provide_to(repo(&l))
      .merge(stuck);
    let provided = use_repo(&l).provide_layer(layer);
    let eff = provided.fork().flat_map({
      let go = Arc::clone(&go);
      move |h| started(&go).then(h.interrupt())
    });
    let Exit::Success(ex) = d.run(eff) else {
      panic!("{d:?}: root failed")
    };
    assert!(
      matches!(ex, Exit::Failure(Cause::Interrupt(_))),
      "{d:?}: {ex:?}"
    );
    assert_eq!(
      entries(&l),
      strings(&[
        "acquire pool",
        "acquire repo",
        "release repo Interrupt",
        "release pool Interrupt"
      ]),
      "{d:?}"
    );
  });
}

#[test]
fn launch_holds_the_layer_until_interrupted() {
  on_both(|d| {
    let (l, go) = (log(), flag());
    let g = Arc::clone(&go);
    let marker: Layer<HCons<Cache, HNil>, Fail> =
      Layer::effect(move || set(&g).map(|()| hlist![Cache]));
    let layer = resource(&l, "pool", || Pool).merge(marker);
    let launched: Effect<Never, Fail> = layer.launch();
    let eff = launched.fork().flat_map({
      let (go, l) = (Arc::clone(&go), Arc::clone(&l));
      move |h| {
        started(&go)
          .then(push(&l, "still held"))
          .then(h.interrupt())
      }
    });
    let Exit::Success(ex) = d.run(eff) else {
      panic!("{d:?}: root failed")
    };
    assert!(
      matches!(ex, Exit::Failure(Cause::Interrupt(_))),
      "{d:?}: {ex:?}"
    );
    assert_eq!(
      entries(&l),
      strings(&["acquire pool", "still held", "release pool Interrupt"]),
      "{d:?}"
    );
  });
}

// ── AC2: fresh() builds twice ───────────────────────────────────────────────────────────────

struct Users;
struct Orders;

fn users() -> Layer<HCons<Users, HNil>, Fail, HCons<Pool, HNil>> {
  Layer::effect(|| need::<Pool>().map(|_| hlist![Users]))
}

fn orders() -> Layer<HCons<Orders, HNil>, Fail, HCons<Pool, HNil>> {
  Layer::effect(|| need::<Pool>().map(|_| hlist![Orders]))
}

#[test]
fn a_shared_layer_is_built_and_released_once() {
  on_both(|d| {
    let l = log();
    let pool = resource(&l, "pool", || Pool);
    let both = pool
      .clone()
      .provide_to(users())
      .merge(pool.provide_to(orders()));
    let eff = uses2::<Users, Orders>().provide_layer(both);
    assert!(matches!(d.run(eff), Exit::Success(_)), "{d:?}");
    assert_eq!(
      entries(&l),
      strings(&["acquire pool", "release pool Success"]),
      "{d:?}"
    );
  });
}

#[test]
fn fresh_builds_twice_and_releases_both() {
  on_both(|d| {
    let l = log();
    let pool = resource(&l, "pool", || Pool);
    let both = pool
      .clone()
      .provide_to(users())
      .merge(pool.fresh().provide_to(orders()));
    let eff = uses2::<Users, Orders>().provide_layer(both);
    assert!(matches!(d.run(eff), Exit::Success(_)), "{d:?}");
    assert_eq!(
      entries(&l),
      strings(&[
        "acquire pool",
        "acquire pool",
        "release pool Success",
        "release pool Success"
      ]),
      "{d:?}"
    );
  });
}

#[test]
fn fresh_alone_still_builds_once_per_build() {
  let built = Arc::new(AtomicUsize::new(0));
  let b = Arc::clone(&built);
  let layer: Layer<HCons<Pool, HNil>, Fail> = Layer::effect(move || {
    let b = Arc::clone(&b);
    sync(move || {
      b.fetch_add(1, Ordering::SeqCst);
      hlist![Pool]
    })
  })
  .fresh();
  for _ in 0..2 {
    let eff = uses::<Pool>().provide_layer(layer.clone());
    assert!(matches!(Driver::Test.run(eff), Exit::Success(_)));
  }
  assert_eq!(
    built.load(Ordering::SeqCst),
    2,
    "one construction per build"
  );
}

// ── AC3: finalizers are async ───────────────────────────────────────────────────────────────

#[test]
fn finalizers_are_async_effects_and_keep_their_order() {
  on_both(|d| {
    let l = log();
    let slow = |name: &'static str, l: &Log| -> Layer<HCons<Pool, HNil>, Fail> {
      let (la, lr) = (Arc::clone(l), Arc::clone(l));
      Layer::acquire_release(
        move || push(&la, format!("acquire {name}")).map(|()| hlist![Pool]),
        move |_, _| {
          let lr = Arc::clone(&lr);
          sleep::<Never, HNil>(Duration::from_millis(20))
            .then(yield_fiber())
            .then(push(&lr, format!("released {name} after sleeping")))
        },
      )
    };
    let quick = resource(&l, "cache", || Cache);
    let eff = uses2::<Pool, Cache>().provide_layer(slow("pool", &l).merge(quick));
    assert!(matches!(d.run(eff), Exit::Success(_)), "{d:?}");
    assert_eq!(
      entries(&l),
      strings(&[
        "acquire pool",
        "acquire cache",
        "release cache Success",
        "released pool after sleeping"
      ]),
      "{d:?}"
    );
  });
}

#[test]
fn an_async_finalizer_is_awaited_before_the_provided_effect_completes() {
  on_both(|d| {
    let l = log();
    let lr = Arc::clone(&l);
    let layer: Layer<HCons<Pool, HNil>, Fail> = Layer::acquire_release(
      || succeed(hlist![Pool]),
      move |_, _| {
        let lr = Arc::clone(&lr);
        sleep::<Never, HNil>(Duration::from_millis(20)).then(push(&lr, "closed"))
      },
    );
    let l2 = Arc::clone(&l);
    let eff = uses::<Pool>()
      .provide_layer(layer)
      .flat_map(move |_| push(&l2, "after provide"));
    assert!(matches!(d.run(eff), Exit::Success(())), "{d:?}");
    assert_eq!(entries(&l), strings(&["closed", "after provide"]), "{d:?}");
  });
}
