//! Kernel semantics for spike B: fibers, interrupts, masks, finalizers, panics, executors,
//! `LocalEffect`, and two inference probes. Not part of the corpus count.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use kernel_b::{
  Cause, Effect, Env, Has, LocalEffect, Never, Runtime, block_on, effect, effect_local, sleep,
  succeed,
};
use spike_corpus::{Db, DbError, Logger};

#[test]
fn ready_binds_do_not_allocate_a_fiber() {
  let e: Effect<u32, (), ()> = succeed(1).map(|n| n + 1).flat_map(|n| succeed(n * 10));
  assert!(e.is_ready());
  assert_eq!(e.block_on(), Ok(20));
}

#[test]
fn deep_left_nested_async_chain_runs() {
  let mut e: Effect<u64, (), ()> = succeed(0);
  for _ in 0..10_000 {
    e = e.flat_map(|n| kernel_b::from_async(async move { Ok(n + 1) }));
  }
  // 10k nested boxed futures are polled recursively (no trampoline). Measured: this overflows a
  // 2 MiB stack (the tokio worker default), so the test runs it on a 64 MiB thread. RISK, see
  // SPIKE-NOTES.md.
  let out = std::thread::Builder::new()
    .stack_size(64 << 20)
    .spawn(move || e.block_on())
    .expect("thread")
    .join()
    .expect("join");
  assert_eq!(out, Ok(10_000));
}

#[test]
fn fork_join_and_structured_children() {
  let started = Arc::new(AtomicU32::new(0));
  let s2 = Arc::clone(&started);
  let e: Effect<u32, (), ()> = effect!(|r: &mut ()| {
    let a = ~ succeed::<u32, (), ()>(20).fork();
    let b = ~ succeed::<u32, (), ()>(22).fork();
    // A child left running is interrupted when the parent exits.
    let s3 = Arc::clone(&s2);
    let lingering: Effect<(), (), ()> = effect!(|r: &mut ()| {
      s3.fetch_add(1, Ordering::SeqCst);
      ~ sleep::<(), ()>(Duration::from_secs(30));
      s3.fetch_add(100, Ordering::SeqCst);
    });
    ~ lingering.fork();
    ~ kernel_b::yield_now::<(), ()>();
    let x = ~ a.join();
    let y = ~ b.join();
    x + y
  });
  let t = Instant::now();
  assert_eq!(e.block_on(), Ok(42));
  assert!(
    t.elapsed() < Duration::from_secs(5),
    "child was not interrupted"
  );
  assert_eq!(started.load(Ordering::SeqCst), 1);
}

#[test]
fn interrupt_latency_of_a_sleeping_fiber() {
  // p50/p99 of fork(sleep 10s) -> interrupt -> exit observed, on the ParkExecutor.
  let mut samples = Vec::new();
  for _ in 0..200 {
    let e: Effect<Duration, (), ()> = effect!(|r: &mut ()| {
      let h = ~ sleep::<(), ()>(Duration::from_secs(10)).fork();
      ~ kernel_b::yield_now::<(), ()>();
      let t = Instant::now();
      let exit = ~ h.interrupt();
      assert!(matches!(exit, Err(Cause::Interrupt(_))));
      t.elapsed()
    });
    samples.push(e.block_on().expect("interrupt"));
  }
  samples.sort();
  let p50 = samples[samples.len() / 2];
  let p99 = samples[samples.len() * 99 / 100];
  println!("interrupt latency p50={p50:?} p99={p99:?}");
  assert!(
    p99 < Duration::from_millis(1),
    "falsifier: p99 {p99:?} >= 1ms"
  );
}

#[test]
fn finalizers_run_on_interrupt_and_masks_hold() {
  let finalized = Arc::new(AtomicBool::new(false));
  let masked_done = Arc::new(AtomicBool::new(false));
  let (f2, m2) = (Arc::clone(&finalized), Arc::clone(&masked_done));
  let e: Effect<bool, (), ()> = effect!(|r: &mut ()| {
    let f3 = Arc::clone(&f2);
    let victim = sleep::<(), ()>(Duration::from_secs(30)).ensuring(effect!(|r: &mut ()| {
      f3.store(true, Ordering::SeqCst);
    }));
    let h = ~ victim.fork();
    let m3 = Arc::clone(&m2);
    let critical: Effect<(), (), ()> = effect!(|r: &mut ()| {
      ~ sleep::<(), ()>(Duration::from_millis(20));
      m3.store(true, Ordering::SeqCst);
    });
    let masked = ~ critical.uninterruptible().fork();
    ~ kernel_b::yield_now::<(), ()>();
    let exit = ~ h.interrupt();
    let masked_exit = ~ masked.interrupt();
    exit.is_err() && masked_exit.is_ok()
  });
  assert_eq!(e.block_on(), Ok(true));
  assert!(
    finalized.load(Ordering::SeqCst),
    "ensuring finalizer skipped"
  );
  assert!(
    masked_done.load(Ordering::SeqCst),
    "masked region was interrupted"
  );
}

#[test]
fn panic_becomes_die_with_payload() {
  // A runtime argument, so the payload is a formatted `String` (rustc folds literal args into
  // a `&'static str` payload).
  let n = std::hint::black_box(7);
  let e: Effect<u32, (), ()> = effect!(|r: &mut ()| {
    ~ kernel_b::yield_now::<(), ()>();
    if r == &mut () {
      panic!("boom {n}");
    }
    1
  });
  match e.block_on() {
    Err(Cause::Die(d)) => {
      assert_eq!(d.message(), "boom 7");
      let payload = d.payload().expect("payload kept");
      assert_eq!(
        payload.downcast_ref::<String>().map(String::as_str),
        Some("boom 7"),
        "{d:?}"
      );
    }
    other => panic!("expected Die, got {other:?}"),
  }
}

#[test]
fn block_on_inside_a_fiber_is_a_loud_defect() {
  let e: Effect<u32, (), ()> = effect!(|r: &mut ()| {
    ~ kernel_b::yield_now::<(), ()>();
    block_on(succeed::<u32, (), ()>(1).map(|n| n + 1).flat_map(|n| {
      kernel_b::from_async(async move { Ok(n) })
    }))
    .unwrap_or(0)
  });
  match e.block_on() {
    Err(Cause::Die(d)) => assert!(d.message().contains("inside a fiber"), "{}", d.message()),
    other => panic!("expected Die, got {other:?}"),
  }
}

#[test]
fn sleeping_fibers_do_not_spin() {
  let rt = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(4)
    .enable_all()
    .build()
    .expect("tokio");
  let krt = Runtime::tokio(rt.handle().clone());
  let cpu = cpu_time::ProcessTime::now();
  let wall = Instant::now();
  rt.block_on(async {
    let hs: Vec<_> = (0..1000)
      .map(|_| krt.spawn(sleep::<(), ()>(Duration::from_millis(50)), ()))
      .collect();
    for h in hs {
      assert!(h.exit().await.is_ok());
    }
  });
  let (cpu, wall) = (cpu.elapsed(), wall.elapsed());
  println!("1000 x sleep(50ms) on tokio: wall={wall:?} cpu={cpu:?}");
  assert!(cpu < wall * 2, "fibers spin: cpu {cpu:?} vs wall {wall:?}");
}

#[test]
fn tokio_runtime_runs_effects_with_env() {
  let rt = tokio::runtime::Runtime::new().expect("tokio");
  let krt = Runtime::tokio(rt.handle().clone());
  let out = rt.block_on(krt.run(count::<Db>(), Db::with_rows("users", 2)));
  assert_eq!(out, Ok(2));
}

fn count<R: Has<Db>>() -> Effect<usize, DbError, R> {
  effect!(|r| ~Db.count("users").await?)
}

#[test]
fn local_effect_holds_rc_across_await() {
  let e: LocalEffect<usize, Never, ()> = effect_local!(|r| {
    let shared = Rc::new(5usize);
    ~ LocalEffect::<(), Never, ()>::from_async(async { Ok(()) });
    *shared + 1
  });
  assert_eq!(e.block_on(), Ok(6));
  let lifted: LocalEffect<u32, (), ()> = succeed::<u32, (), ()>(3).into();
  assert_eq!(lifted.block_on(), Ok(3));
}

#[test]
fn zip_par_fails_fast_and_interrupts_the_sibling() {
  let slow_finished = Arc::new(AtomicBool::new(false));
  let s2 = Arc::clone(&slow_finished);
  let slow: Effect<u32, &'static str, ()> = effect!(|r: &mut ()| {
    ~ sleep::<&'static str, ()>(Duration::from_secs(30));
    s2.store(true, Ordering::SeqCst);
    1
  });
  let failing: Effect<u32, &'static str, ()> = kernel_b::fail("boom");
  let t = Instant::now();
  assert_eq!(slow.zip_par(failing).block_on(), Err(Cause::Fail("boom")));
  assert!(t.elapsed() < Duration::from_secs(5));
  assert!(!slow_finished.load(Ordering::SeqCst));
}

// ── inference probes (reported in SPIKE-NOTES) ──────────────────────────────────────────────

#[derive(Clone, Env)]
struct ProbeEnv {
  db: Db,
  logger: Logger,
}

fn count_len<R: Has<Db>>() -> Effect<usize, DbError, R> {
  effect!(|r| {
    let n = ~ count();
    n.pow(1)
  })
}

/// With a concrete `Env` (`ProbeEnv`), binding a *generic* helper is ambiguous between the
/// same-`R` and the env-free `IntoBind` impls, so the helper's `R` must be named.
fn concrete_env_handler() -> Effect<usize, DbError, ProbeEnv> {
  effect!(|r| {
    ~Logger.info("probe");
    ~ count::<ProbeEnv>() // ann: E0283 without it (same-R vs env-free IntoBind impls)
  })
}

#[test]
fn inference_probes_compile_with_the_annotations_shown() {
  let env = ProbeEnv {
    db: Db::with_rows("users", 4),
    logger: Logger::default(),
  };
  assert_eq!(count_len().provide(env.clone()).block_on(), Ok(4));
  assert_eq!(concrete_env_handler().provide(env).block_on(), Ok(4));
}
