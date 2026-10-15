//! `k-typed-r` acceptance tests for the `id_effect::next` kernel: `need`, `provide*`, `narrow`,
//! `Ctx::get` / `Ctx::bind`, and the runtime-checked `DynEnv`.
//!
//! Effects are driven with `pollster::block_on` over a test-local `Executor` stub.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use id_effect::next::{
  BindFuture, BoxFuture, DynEnv, Effect, Executor, FiberContext, HCons, HNil, LocalBoxFuture,
  MissingCapability, NotLocal, die, fail, from_async, hlist, need, succeed, sync, try_need,
};
use id_effect::{Cause, Exit};

struct StubExec;

impl Executor for StubExec {
  fn spawn(&self, _: BoxFuture<'static, ()>) {
    unreachable!("next_typed_r tests spawn no tasks")
  }
  fn spawn_local(&self, _: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal> {
    Err(NotLocal)
  }
  fn spawn_blocking(&self, _: Box<dyn FnOnce() + Send>) {
    unreachable!("next_typed_r tests spawn no blocking work")
  }
  fn sleep(&self, _: Duration) -> BoxFuture<'static, ()> {
    Box::pin(async {})
  }
  fn now(&self) -> Instant {
    Instant::now()
  }
}

fn fiber() -> Arc<FiberContext> {
  FiberContext::new_root(Arc::new(StubExec))
}

fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
  pollster::block_on(eff.run_on(fiber()))
}

fn assert_send<T: Send>() {}

#[derive(Debug)]
struct Db(u32);
#[derive(Debug)]
struct Logger(&'static str);
#[derive(Debug)]
struct Cache(u8);

type DbLog = HCons<Db, HCons<Logger, HNil>>;

/// An erased (`Op`) effect that reads `Db` and `Logger` from its `Ctx`.
fn describe() -> Effect<String, (), DbLog> {
  Effect::from_ctx(|ctx| async move {
    Ok(format!(
      "{}:{}",
      ctx.get::<Db, _>().0,
      ctx.get::<Logger, _>().0
    ))
  })
}

// ── need ────────────────────────────────────────────────────────────────────────────────────

#[test]
fn need_reads_a_capability_with_effect_map_and_flat_map() {
  let e: Effect<Arc<Db>, (), DbLog> = need::<Db>().effect();
  let e = e.map(|db| db.0).provide(Db(3)).provide(Logger("l"));
  assert_eq!(run(e), Exit::Success(3));

  let m: Effect<u32, (), DbLog> = need::<Logger>().map(|l| l.0.len() as u32);
  assert_eq!(
    run(m.provide_all(hlist![Db(0), Logger("four")])),
    Exit::Success(4)
  );

  let fm: Effect<u32, (), DbLog> =
    need::<Db>().flat_map(|db| need::<Logger>().map(move |l| db.0 + l.0.len() as u32));
  assert_eq!(
    run(fm.provide_all(hlist![Db(10), Logger("ab")])),
    Exit::Success(12)
  );
}

macro_rules! caps16 {
  ($($c:ident),*) => {
    $(#[derive(Debug)] #[allow(dead_code)] struct $c(u32);)*
    type R16 = caps16!(@t $($c),*);
    fn env16() -> R16 { caps16!(@v 1u32; $($c),*) }
  };
  (@t) => { HNil };
  (@t $h:ident $(, $t:ident)*) => { HCons<$h, caps16!(@t $($t),*)> };
  (@v $n:expr;) => { HNil };
  (@v $n:expr; $h:ident $(, $t:ident)*) => { HCons::new($h($n), caps16!(@v $n + 1; $($t),*)) };
}

caps16!(
  C1, C2, C3, C4, C5, C6, C7, C8, C9, C10, C11, C12, C13, C14, C15, C16
);

#[test]
fn need_over_sixteen_capabilities_reads_the_last_one() {
  let e: Effect<u32, (), R16> = need::<C16>().map(|c| c.0);
  assert_eq!(run(e.provide_all(env16())), Exit::Success(16));
  let first: Effect<u32, (), R16> = need::<C1>().map(|c| c.0);
  assert_eq!(run(first.provide_all(env16())), Exit::Success(1));
  let mid: Effect<u32, (), R16> = need::<C8>().map(|c| c.0);
  assert_eq!(run(mid.provide_all(env16())), Exit::Success(8));
}

// ── provide ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn provide_removes_exactly_one_capability() {
  let rest: Effect<String, (), HCons<Logger, HNil>> = describe().provide(Db(7));
  let none: Effect<String, ()> = rest.provide(Logger("log"));
  assert_eq!(run(none), Exit::Success("7:log".to_owned()));

  // The other order, on a fused (`Sync`) effect.
  let fused: Effect<u32, (), DbLog> = need::<Db>().map(|d| d.0);
  let rest: Effect<u32, (), HCons<Db, HNil>> = fused.provide(Logger("unused"));
  assert_eq!(run(rest.provide(Db(9))), Exit::Success(9));
}

#[test]
fn provide_on_a_ready_effect_keeps_it_ready() {
  let ready: Effect<u8, (), HCons<Db, HNil>> = succeed(5);
  let e: Effect<u8, ()> = ready.provide(Db(0));
  assert_eq!(format!("{e:?}"), "Effect::Pure");
  assert_eq!(run(e), Exit::Success(5));
}

#[test]
fn provide_some_eliminates_a_sub_list_in_any_order() {
  let e: Effect<String, (), HCons<Cache, DbLog>> = need::<Cache>()
    .effect()
    .flat_map(|c| describe().narrow().map(move |s| format!("{s}:{}", c.0)));
  let rest: Effect<String, (), HCons<Db, HNil>> = e.provide_some(hlist![Logger("x"), Cache(2)]);
  assert_eq!(run(rest.provide(Db(1))), Exit::Success("1:x:2".to_owned()));
}

#[test]
fn provide_all_and_provide_env() {
  assert_eq!(
    run(describe().provide_all(hlist![Db(1), Logger("a")])),
    Exit::Success("1:a".to_owned())
  );
  // A wider application env, in another order, with an extra capability.
  let app = hlist![Cache(0), Logger("b"), Db(2)];
  assert_eq!(
    run(describe().provide_env(app)),
    Exit::Success("2:b".to_owned())
  );
}

#[test]
fn provide_restores_the_outer_environment_afterwards() {
  // The inner `Db(1)` shadows nothing: after the provided region the outer env is back.
  let inner: Effect<u32, (), HCons<Db, HNil>> =
    Effect::from_ctx(|ctx| async move { Ok(ctx.get::<Db, _>().0) });
  let outer: Effect<u32, (), HCons<Logger, HNil>> = inner.provide(Db(1)).narrow().flat_map(|n| {
    Effect::from_ctx(move |ctx| async move { Ok(n + ctx.get::<Logger, _>().0.len() as u32) })
  });
  assert_eq!(run(outer.provide(Logger("xyz"))), Exit::Success(4));
}

// ── narrow ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn narrow_lifts_into_a_wider_environment() {
  let small: Effect<String, (), DbLog> = describe();
  type Wide = HCons<Cache, HCons<Logger, HCons<Db, HNil>>>;
  let wide: Effect<String, (), Wide> = small.narrow();
  assert_eq!(
    run(wide.provide_all(hlist![Cache(0), Logger("w"), Db(4)])),
    Exit::Success("4:w".to_owned())
  );
  // Fused effects narrow without leaving the fast path.
  let fused: Effect<u32, (), HCons<Db, HNil>> = need::<Db>().map(|d| d.0);
  let wide: Effect<u32, (), DbLog> = fused.narrow();
  assert_eq!(format!("{wide:?}"), "Effect::Sync");
  assert_eq!(
    run(wide.provide_all(hlist![Db(6), Logger("")])),
    Exit::Success(6)
  );
}

#[test]
fn narrow_to_the_same_environment_is_free() {
  // An `Op` effect that reports the address of the environment it runs under.
  fn env_addr() -> Effect<usize, (), DbLog> {
    Effect::from_ctx(|ctx| async move { Ok(ctx.env() as *const DbLog as usize) })
  }
  let e: Effect<(usize, usize), (), DbLog> = env_addr().zip(env_addr().narrow::<DbLog, _>());
  let (outer, narrowed) = match run(e.provide_all(hlist![Db(0), Logger("")])) {
    Exit::Success(p) => p,
    other => panic!("{other:?}"),
  };
  assert_eq!(outer, narrowed, "no projection: the same environment value");

  // A real narrowing projects a new environment value.
  let sub: Effect<usize, (), HCons<Logger, HCons<Db, HNil>>> =
    Effect::from_ctx(|ctx| async move { Ok(ctx.env() as *const _ as usize) });
  let e: Effect<(usize, usize), (), DbLog> = env_addr().zip(sub.narrow());
  let (outer, projected) = match run(e.provide_all(hlist![Db(0), Logger("")])) {
    Exit::Success(p) => p,
    other => panic!("{other:?}"),
  };
  assert_ne!(outer, projected);
}

#[test]
fn hnil_embeds_in_anything() {
  let ready: Effect<u8, (), HNil> = succeed(1);
  let erased: Effect<u8, (), HNil> = from_async(|| async { Ok(2) });
  let fused: Effect<u8, (), HNil> = sync(|| 3);
  let wide: Effect<u8, (), DbLog> = ready
    .narrow()
    .flat_map(|a| erased.narrow().map(move |b| a + b))
    .flat_map(|ab| fused.narrow().map(move |c| ab + c));
  assert_eq!(
    run(wide.provide_all(hlist![Db(0), Logger("")])),
    Exit::Success(6)
  );
}

// ── Ctx::get / Ctx::bind ────────────────────────────────────────────────────────────────────

#[test]
fn bind_futures_and_bodies_are_send() {
  assert_send::<BindFuture<String, ()>>();
  assert_send::<Effect<String, (), DbLog>>();
}

#[test]
fn bind_runs_a_sub_effect_over_a_subset_of_the_environment() {
  let log_only: Effect<usize, (), HCons<Logger, HNil>> = need::<Logger>().map(|l| l.0.len());
  let body: Effect<String, (), HCons<Cache, DbLog>> = Effect::from_ctx(|ctx| async move {
    let s = ctx.bind(describe()).await?;
    let n = ctx.bind(log_only).await?;
    let c = ctx.get::<Cache, _>().0;
    Ok(format!("{s}/{n}/{c}"))
  });
  assert_eq!(
    run(body.provide_all(hlist![Cache(3), Db(1), Logger("ab")])),
    Exit::Success("1:ab/2/3".to_owned())
  );
}

#[test]
fn bind_of_ready_and_fused_effects_needs_no_runloop() {
  let body: Effect<u32, (), DbLog> = Effect::from_ctx(|ctx| async move {
    let a = ctx.bind(succeed::<u32, (), HNil>(1)).await?;
    // The sub-effect's `R` must be known (a concrete helper type, as `caps_next!` gives).
    let read_db: Effect<u32, (), HCons<Db, HNil>> = need::<Db>().map(|d| d.0);
    let b = ctx.bind(read_db).await?;
    Ok(a + b)
  });
  assert_eq!(
    run(body.provide_all(hlist![Db(41), Logger("")])),
    Exit::Success(42)
  );
}

#[test]
fn bind_surfaces_typed_errors_to_question_mark() {
  let after = Arc::new(AtomicUsize::new(0));
  let a2 = Arc::clone(&after);
  let body: Effect<u8, &'static str, HCons<Db, HNil>> = Effect::from_ctx(move |ctx| async move {
    let _: u8 = ctx.bind(fail::<u8, &'static str, HNil>("nope")).await?;
    a2.fetch_add(1, Ordering::SeqCst);
    Ok(0)
  });
  assert_eq!(run(body.provide(Db(0))), Exit::Failure(Cause::Fail("nope")));
  assert_eq!(after.load(Ordering::SeqCst), 0);
}

#[test]
fn bind_resumes_defects_as_the_same_die() {
  let body: Effect<u8, (), HCons<Db, HNil>> = Effect::from_ctx(|ctx| async move {
    let sub: Effect<u8, (), HCons<Db, HNil>> =
      from_async(|| async { Ok(0) }).flat_map(|_: u8| die("sub died"));
    ctx.bind(sub).await
  });
  match run(body.provide(Db(0))) {
    Exit::Failure(Cause::Die(d)) => assert_eq!(d.message(), "sub died"),
    other => panic!("expected Die, got {other:?}"),
  }
}

#[test]
fn interrupting_the_parent_interrupts_a_suspended_bind_and_runs_its_finalizer() {
  let fiber = fiber();
  let (tx, rx) = mpsc::channel::<()>();
  let tx = Mutex::new(Some(tx));
  let finalized = Arc::new(AtomicUsize::new(0));
  let fin2 = Arc::clone(&finalized);
  let sub: Effect<u8, (), HCons<Logger, HNil>> = from_async(move || {
    let started = tx.lock().unwrap().take();
    async move {
      if let Some(t) = started {
        let _ = t.send(());
      }
      std::future::pending::<Result<u8, ()>>().await
    }
  })
  .ensuring(sync(move || {
    fin2.fetch_add(1, Ordering::SeqCst);
  }));
  let after = Arc::new(AtomicUsize::new(0));
  let a2 = Arc::clone(&after);
  let body: Effect<u8, (), DbLog> = Effect::from_ctx(move |ctx| async move {
    let v = ctx.bind(sub).await?;
    a2.fetch_add(1, Ordering::SeqCst);
    Ok(v)
  });
  let runnable = body.provide_all(hlist![Db(0), Logger("")]);
  let f2 = Arc::clone(&fiber);
  let driver = std::thread::spawn(move || pollster::block_on(runnable.run_on(f2)));
  rx.recv().expect("sub-effect started");
  fiber.request_interrupt();
  let exit = driver.join().expect("driver");
  assert_eq!(exit, Exit::Failure(Cause::Interrupt(fiber.id())));
  assert_eq!(after.load(Ordering::SeqCst), 0, "the body did not continue");
  assert_eq!(
    finalized.load(Ordering::SeqCst),
    1,
    "the sub-effect's finalizer ran"
  );
}

// ── DynEnv ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn dyn_env_insert_get_remove() {
  let mut env = DynEnv::new();
  assert!(env.is_empty());
  env.insert(Db(1)).insert(Logger("l"));
  env.insert(Db(2)); // replaces
  assert_eq!(env.len(), 2);
  assert!(env.contains::<Db>());
  assert_eq!(env.try_get::<Db>().map(|d| d.0), Ok(2));
  let err = env.try_get::<Cache>().unwrap_err();
  assert_eq!(err, MissingCapability::of::<Cache>());
  assert!(err.to_string().contains("Cache"), "{err}");
  assert_eq!(env.remove::<Db>().map(|d| d.0), Some(2));
  assert!(env.remove::<Db>().is_none());
  let shared = Arc::new(Cache(9));
  let env = env.with(Db(3));
  let mut env = env;
  env.insert_arc(Arc::clone(&shared));
  assert!(Arc::ptr_eq(&env.try_get::<Cache>().unwrap(), &shared));
  let dbg = format!("{env:?}");
  assert!(dbg.contains("Cache") && dbg.contains("Logger"), "{dbg}");
}

#[test]
fn try_need_is_fallible_and_never_panics() {
  let found = try_need::<Db>()
    .map(|d| d.0)
    .provide(DynEnv::new().with(Db(5)));
  assert_eq!(run(found), Exit::Success(5));

  let missing = try_need::<Cache>().map(|c| c.0).provide(DynEnv::new());
  assert_eq!(
    run(missing),
    Exit::Failure(Cause::Fail(MissingCapability::of::<Cache>()))
  );

  // `DynEnv` is an ordinary capability: it narrows into a larger typed environment.
  let mixed: Effect<u32, MissingCapability, HCons<Db, HCons<DynEnv, HNil>>> = need::<Db>()
    .flat_map(|db| {
      try_need::<Cache>()
        .narrow()
        .map(move |c| db.0 + u32::from(c.0))
    });
  assert_eq!(
    run(mixed.provide_all(hlist![Db(1), DynEnv::new().with(Cache(2))])),
    Exit::Success(3)
  );
}
