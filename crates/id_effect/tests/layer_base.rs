//! `k-layer-base` acceptance tests for `id_effect::Layer`: `succeed`/`from_effect`/
//! `build`/`merge`/`provide_merge`, `Effect::provide_layer`, shared-input deduplication and the
//! duplicate-output rejection (H1 condition 2).
//!
//! Effects are driven with `pollster::block_on` over a test-local `Executor` stub.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use id_effect::{
  BoxFuture, Effect, Executor, FiberContext, HCons, HNil, Layer, LocalBoxFuture, NotLocal, fail,
  hlist, need, sync,
};
use id_effect::{Cause, Exit};

struct StubExec;

impl Executor for StubExec {
  fn spawn(&self, _: BoxFuture<'static, ()>) {
    unreachable!("next_layer_base tests spawn no tasks")
  }
  fn spawn_local(&self, _: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal> {
    Err(NotLocal)
  }
  fn spawn_blocking(&self, _: Box<dyn FnOnce() + Send>) {
    unreachable!("next_layer_base tests spawn no blocking work")
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

struct ConfigSource(&'static str);
struct Logger(&'static str);
struct Db {
  url: &'static str,
  cfg: Arc<ConfigSource>,
}
struct Sessions;
struct Cache;
struct Mailer {
  relay: &'static str,
  cfg: Arc<ConfigSource>,
}
struct Greeting(&'static str);
struct Counter(u32);
struct Hits;
struct Tenant(&'static str);

type Cfg = HCons<ConfigSource, HNil>;

/// The spike's failing case, half one: needs `ConfigSource`, produces `Db`.
fn db_layer() -> Layer<HCons<Db, HNil>, AppError, Cfg> {
  Layer::from_effect(need::<ConfigSource>().map(|cfg| {
    hlist![Db {
      url: cfg.0,
      cfg: Arc::clone(&cfg),
    }]
  }))
}

/// The spike's failing case, half two: also needs `ConfigSource`, produces `Mailer`.
fn mail_layer() -> Layer<HCons<Mailer, HNil>, AppError, Cfg> {
  Layer::from_effect(need::<ConfigSource>().map(|cfg| {
    hlist![Mailer {
      relay: "smtp",
      cfg: Arc::clone(&cfg),
    }]
  }))
}

// ── AC1: constructors and build ─────────────────────────────────────────────────────────────

#[test]
fn succeed_and_from_effect_build_their_outputs() {
  let env = ok(Layer::<_, AppError, HNil>::succeed(hlist![Greeting("hi"), Counter(2)]).build());
  assert_eq!((env.0.0, env.1.0.0), ("hi", 2));

  let built = AtomicUsize::new(0);
  let built = Arc::new(built);
  let seen = Arc::clone(&built);
  let layer: Layer<HCons<Counter, HNil>, AppError, HNil> = Layer::from_effect(sync(move || {
    hlist![Counter(seen.fetch_add(1, Ordering::SeqCst) as u32)]
  }));
  assert_eq!(built.load(Ordering::SeqCst), 0, "layers are lazy");
  assert_eq!(ok(layer.build()).0.0, 0);
  assert_eq!(built.load(Ordering::SeqCst), 1);
}

#[test]
fn failing_layer_fails_the_merged_build() {
  let bad: Layer<HCons<Hits, HNil>, AppError, HNil> = Layer::from_effect(fail(AppError::Boom));
  let merged = Layer::succeed(hlist![Greeting("hi")]).merge(bad);
  match run(merged.build()) {
    Exit::Failure(Cause::Fail(e)) => assert_eq!(e, AppError::Boom),
    _ => panic!("expected Fail(Boom)"),
  }
}

// ── AC2: shared inputs, the spike's E0283 case ──────────────────────────────────────────────

#[test]
fn merge_with_a_shared_input_needs_no_annotations_and_shares_the_arc() {
  // Spike `tests/ui/duplicate_capability_layer.rs`: E0283 there, 0 annotations here.
  let infra = db_layer().merge(mail_layer());

  let cfg_env = HCons::new(ConfigSource("mem://app"), HNil);
  let cfg = Arc::clone(&cfg_env.0);
  let env = ok(infra.build().provide_env(cfg_env));

  let (db, mailer) = (&env.0, &env.1.0);
  assert_eq!((db.url, mailer.relay), ("mem://app", "smtp"));
  assert!(Arc::ptr_eq(&db.cfg, &mailer.cfg));
  assert!(Arc::ptr_eq(&db.cfg, &cfg));
}

#[test]
fn shared_input_supplied_by_provide_merge_is_one_arc() {
  let base = Layer::succeed(hlist![ConfigSource("mem://x"), Logger("l")]);
  let env = ok(db_layer().merge(mail_layer()).provide_merge(base).build());
  let (cfg, db, mailer) = (&env.0, &env.1.1.0, &env.1.1.1.0);
  assert!(Arc::ptr_eq(cfg, &db.cfg));
  assert!(Arc::ptr_eq(cfg, &mailer.cfg));
}

#[test]
fn provide_layer_runs_an_effect_against_a_merged_layer() {
  fn relay_and_url() -> Effect<String, AppError, HCons<Mailer, HCons<Db, HNil>>> {
    need::<Mailer>().flat_map(|m| need::<Db>().map(move |db| format!("{}@{}", m.relay, db.url)))
  }
  let cfg = Layer::succeed(hlist![ConfigSource("mem://p")]);
  let layer = db_layer().merge(mail_layer()).provide_merge(cfg);
  assert_eq!(ok(relay_and_url().provide_layer(layer)), "smtp@mem://p");

  // Against a layer that still needs its (shared) input: the result needs that input, once per
  // side, and one `provide_env` feeds both positions.
  let open = db_layer().merge(mail_layer());
  let needs_cfg: Effect<String, AppError, HCons<ConfigSource, Cfg>> =
    relay_and_url().provide_layer(open);
  assert_eq!(
    ok(needs_cfg.provide_env(hlist![ConfigSource("mem://q")])),
    "smtp@mem://q"
  );
}

// ── AC3: W01-shaped wiring (10 caps, 3 strata), spike `tests/corpus.rs:562-569` ─────────────

fn base_layer(
  cfg: ConfigSource,
  logger: Logger,
) -> Layer<HCons<ConfigSource, HCons<Logger, HNil>>, AppError> {
  Layer::succeed(hlist![cfg, logger])
}

type Infra = HCons<Db, HCons<Sessions, HCons<Cache, HNil>>>;

fn infra_layer() -> Layer<Infra, AppError, Cfg> {
  Layer::from_effect(need::<ConfigSource>().map(|cfg| {
    let url = if cfg.0.starts_with("mem://") {
      "mem"
    } else {
      "pg"
    };
    hlist![
      Db {
        url,
        cfg: Arc::clone(&cfg)
      },
      Sessions,
      Cache
    ]
  }))
}

type App = HCons<Greeting, HCons<Counter, HCons<Hits, HCons<Tenant, HCons<Mailer, HNil>>>>>;

fn app_layer() -> Layer<App, AppError> {
  Layer::succeed(hlist![
    Greeting("hello from effect-axum"),
    Counter(7),
    Hits,
    Tenant("public"),
    Mailer {
      relay: "smtp",
      cfg: Arc::new(ConfigSource("app")),
    },
  ])
}

type SummaryCaps = HCons<Greeting, HCons<Db, HCons<Logger, HNil>>>;

fn summary() -> Effect<String, AppError, SummaryCaps> {
  need::<Greeting>().flat_map(|g| {
    need::<Db>()
      .flat_map(move |db| need::<Logger>().map(move |l| format!("{}:{}:{}", g.0, db.url, l.0)))
  })
}

type TenantCaps = HCons<Counter, HCons<Tenant, HNil>>;

fn tenant_count() -> Effect<(String, u32), AppError, TenantCaps> {
  need::<Tenant>().flat_map(|t| need::<Counter>().map(move |c| (t.0.to_string(), c.0)))
}

type RestCaps = HCons<Sessions, HCons<Cache, HCons<Hits, Cfg>>>;

#[test]
fn w01_edge_wiring_compiles_with_zero_annotations() {
  let app = infra_layer()
    .provide_merge(base_layer(ConfigSource("mem://app"), Logger("log")))
    .merge(app_layer());
  let env = ok(app.build());

  assert_eq!(
    ok(summary().provide_env(env.clone())),
    "hello from effect-axum:mem:log"
  );
  assert_eq!(
    ok(tenant_count().provide_env(env.clone())),
    ("public".to_string(), 7)
  );
  let rest: Effect<(), AppError, RestCaps> = need::<Sessions>().map(|_| ());
  ok(rest.provide_env(env));

  // The same graph fed straight to an effect.
  let wired = infra_layer()
    .provide_merge(base_layer(ConfigSource("pg://x"), Logger("l2")))
    .merge(app_layer());
  assert_eq!(
    ok(summary().provide_layer(wired)),
    "hello from effect-axum:pg:l2"
  );
}

// ── AC4: duplicate outputs are rejected with the custom message ─────────────────────────────

fn other_db_layer() -> Layer<HCons<Db, HNil>, AppError> {
  Layer::succeed(hlist![Db {
    url: "other",
    cfg: Arc::new(ConfigSource("other")),
  }])
}

#[test]
#[should_panic(expected = "produced by both sides")]
fn merge_rejects_a_duplicate_output() {
  let _ = db_layer().merge(other_db_layer());
}

#[test]
fn duplicate_output_message_names_the_capability_and_the_fix() {
  let msg = std::panic::catch_unwind(|| {
    let _ = Layer::<_, AppError>::succeed(hlist![Greeting("a")])
      .merge(other_db_layer())
      .merge(db_layer());
  })
  .expect_err("merge must reject a duplicate Db")
  .downcast::<String>()
  .expect("panic payload is a String");
  assert_eq!(
    *msg,
    "capability `layer_base::Db` is produced by both sides of Layer::merge; wrap one in a newtype"
  );
}

#[test]
fn provide_merge_rejects_a_capability_produced_by_both_strata() {
  let msg = std::panic::catch_unwind(|| {
    let base = Layer::<_, AppError>::succeed(hlist![ConfigSource("a")]);
    let reprovides: Layer<Cfg, AppError, Cfg> =
      Layer::from_effect(need::<ConfigSource>().map(|c| hlist![ConfigSource(c.0)]));
    let _ = reprovides.provide_merge(base);
  })
  .expect_err("provide_merge must reject a duplicate ConfigSource")
  .downcast::<String>()
  .expect("panic payload is a String");
  assert_eq!(
    *msg,
    "capability `layer_base::ConfigSource` is produced by both sides of Layer::provide_merge; wrap one in a newtype"
  );
}

#[test]
fn a_newtype_resolves_the_duplicate() {
  struct ReplicaDb(Db);
  let replica: Layer<HCons<ReplicaDb, HNil>, AppError> = Layer::succeed(hlist![ReplicaDb(Db {
    url: "replica",
    cfg: Arc::new(ConfigSource("r")),
  })]);
  let env = ok(
    db_layer()
      .merge(replica)
      .build()
      .provide_env(hlist![ConfigSource("mem://p")]),
  );
  assert_eq!((env.0.url, env.1.0.0.url), ("mem://p", "replica"));
}
