//! `c-resource` acceptance tests for `id_effect`: `Pool`, `KeyedPool` and `Cache`.
//!
//! Every behavioural test runs on a multi-thread `TokioExecutor` **and** on the deterministic
//! `TestExecutor` through [`on_both`], except the time-to-live tests, which run on the
//! `TestExecutor` only so that virtual time drives expiry.

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::Never;
use id_effect::{
  Cache, CacheStats, Concurrency, Effect, Executor, ExitKind, KeyedPool, Pool, TestExecutor,
  TokioExecutor, die, fail, fork_all, sleep, succeed, suspend, sync, yield_fiber,
};
use id_effect::{Cause, Exit};

const HOUR: Duration = Duration::from_secs(3600);
const SEC: Duration = Duration::from_secs(1);

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

fn counter() -> Arc<AtomicUsize> {
  Arc::new(AtomicUsize::new(0))
}

fn count(c: &Arc<AtomicUsize>) -> usize {
  c.load(Ordering::SeqCst)
}

/// An acquire that yields 1, 2, 3, … and counts its calls.
fn numbered(c: &Arc<AtomicUsize>) -> impl Fn() -> Effect<u32, Never> + Send + Sync + 'static {
  let c = Arc::clone(c);
  move || {
    let c = Arc::clone(&c);
    sync(move || c.fetch_add(1, Ordering::SeqCst) as u32 + 1)
  }
}

type Log = Arc<Mutex<Vec<u32>>>;

fn log() -> Log {
  Arc::new(Mutex::new(Vec::new()))
}

fn entries(l: &Log) -> Vec<u32> {
  l.lock().expect("log").clone()
}

/// A release hook that records the released items.
fn recording(l: &Log) -> impl Fn(u32) -> Effect<(), Never> + Send + Sync + 'static {
  let l = Arc::clone(l);
  move |a| {
    let l = Arc::clone(&l);
    sync(move || l.lock().expect("log").push(a))
  }
}

/// Yields until `cond` holds (so another fiber has reached a known point).
fn until<E: Send + 'static>(cond: impl Fn() -> bool + Send + Sync + 'static) -> Effect<(), E> {
  fn go<E: Send + 'static>(cond: Arc<dyn Fn() -> bool + Send + Sync>) -> Effect<(), E> {
    suspend(move || {
      if cond() {
        succeed(())
      } else {
        yield_fiber().flat_map(move |()| go(cond))
      }
    })
  }
  go(Arc::new(cond))
}

fn set<E: Send + 'static>(f: &Arc<AtomicBool>) -> Effect<(), E> {
  let f = Arc::clone(f);
  sync(move || f.store(true, Ordering::SeqCst))
}

fn is_set(f: &Arc<AtomicBool>) -> impl Fn() -> bool + Send + Sync + 'static {
  let f = Arc::clone(f);
  move || f.load(Ordering::SeqCst)
}

fn checkout(pool: &Pool<u32>) -> Effect<u32, Never> {
  let p = pool.clone();
  Effect::scoped(move |s| p.get(&s))
}

// ── Pool ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn pool_constructor_is_plain_and_acquires_nothing() {
  let calls = counter();
  let pool: Pool<u32> = Pool::new(3, numbered(&calls));
  assert_eq!(
    (pool.capacity(), pool.in_use(), pool.idle_count()),
    (3, 0, 0)
  );
  assert_eq!(count(&calls), 0);
}

#[test]
fn pool_reuses_a_checked_in_item() {
  on_both(|d| {
    let calls = counter();
    let pool: Pool<u32> = Pool::new(2, numbered(&calls));
    let eff = checkout(&pool).zip(checkout(&pool));
    assert!(matches!(d.run(eff), Exit::Success((1, 1))), "{d:?}");
    assert_eq!(
      count(&calls),
      1,
      "{d:?}: the second checkout reused the idle item"
    );
    assert_eq!((pool.in_use(), pool.idle_count()), (0, 1), "{d:?}");
  });
}

#[test]
fn pool_hands_out_distinct_items_up_to_capacity() {
  on_both(|d| {
    let calls = counter();
    let pool: Pool<u32> = Pool::new(2, numbered(&calls));
    let p = pool.clone();
    let eff: Effect<(u32, u32), Never> = Effect::scoped(move |s| p.get(&s).zip(p.get(&s)));
    assert!(matches!(d.run(eff), Exit::Success((1, 2))), "{d:?}");
    assert_eq!((pool.in_use(), pool.idle_count()), (0, 2), "{d:?}");
  });
}

#[test]
fn pool_checkout_waits_for_capacity_until_a_scope_closes() {
  on_both(|d| {
    let calls = counter();
    let pool: Pool<u32> = Pool::new(1, numbered(&calls));
    let (p, w) = (pool.clone(), pool.clone());
    let eff = Effect::scoped(move |s| {
      p.get(&s).flat_map(move |held| {
        checkout(&w)
          .fork()
          .flat_map(move |h| until(move || w.waiting() == 1).map(move |()| (held, h)))
      })
    })
    .flat_map(|(held, h)| h.join().map(move |got| (held, got)));
    assert!(matches!(d.run(eff), Exit::Success((1, 1))), "{d:?}");
    assert_eq!(
      count(&calls),
      1,
      "{d:?}: the waiter got the checked-in item"
    );
    assert_eq!((pool.in_use(), pool.waiting()), (0, 0), "{d:?}");
  });
}

/// AC: a pooled item is checked back in by a scope finalizer, so interrupting its holder
/// returns it to the pool.
#[test]
fn pool_item_is_checked_in_when_its_holder_is_interrupted() {
  on_both(|d| {
    let (calls, released) = (counter(), log());
    let pool: Pool<u32> = Pool::new(1, numbered(&calls)).with_release(recording(&released));
    let holding = Arc::new(AtomicBool::new(false));
    let p = pool.clone();
    let h_flag = Arc::clone(&holding);
    let holder: Effect<(), Never> =
      Effect::scoped(move |s| p.get(&s).then(set(&h_flag)).then(sleep(HOUR)));
    let p2 = pool.clone();
    let eff = holder
      .fork()
      .flat_map(move |h| until(is_set(&holding)).then(h.interrupt()))
      .flat_map(move |exit| {
        let snapshot = (p2.in_use(), p2.idle_count());
        checkout(&p2).map(move |again| (ExitKind::of(&exit), snapshot, again))
      });
    let exit = d.run(eff);
    assert!(
      matches!(exit, Exit::Success((ExitKind::Interrupt, (0, 1), 1))),
      "{d:?}: {exit:?}"
    );
    assert_eq!(
      count(&calls),
      1,
      "{d:?}: the interrupted holder's item was reused"
    );
    assert!(
      entries(&released).is_empty(),
      "{d:?}: checked in, not released"
    );
  });
}

#[test]
fn an_interrupted_waiter_leaks_no_capacity() {
  on_both(|d| {
    let calls = counter();
    let pool: Pool<u32> = Pool::new(1, numbered(&calls));
    let (p, w) = (pool.clone(), pool.clone());
    let eff = Effect::scoped(move |s| {
      p.get(&s).flat_map(move |_| {
        let w2 = w.clone();
        checkout(&w)
          .fork()
          .flat_map(move |h| until(move || w2.waiting() == 1).then(h.interrupt()))
          .map(move |exit| (ExitKind::of(&exit), w.waiting(), w.in_use()))
      })
    })
    .flat_map({
      let pool = pool.clone();
      move |r| checkout(&pool).map(move |again| (r, again))
    });
    let exit = d.run(eff);
    assert!(
      matches!(exit, Exit::Success(((ExitKind::Interrupt, 0, 1), 1))),
      "{d:?}: {exit:?}"
    );
    assert_eq!((pool.in_use(), pool.waiting()), (0, 0), "{d:?}");
  });
}

#[test]
fn a_failing_acquire_gives_the_capacity_back() {
  on_both(|d| {
    let calls = counter();
    let c = Arc::clone(&calls);
    let pool: Pool<u32, &'static str> = Pool::new(1, move || {
      let c = Arc::clone(&c);
      suspend(move || match c.fetch_add(1, Ordering::SeqCst) {
        0 => fail("connect refused"),
        n => succeed(n as u32),
      })
    });
    let (p1, p2) = (pool.clone(), pool.clone());
    let eff = Effect::scoped(move |s| p1.get(&s))
      .exit()
      .zip(Effect::scoped(move |s| p2.get(&s)));
    let exit = d.run(eff);
    assert!(
      matches!(
        exit,
        Exit::Success((Exit::Failure(Cause::Fail("connect refused")), 1))
      ),
      "{d:?}: {exit:?}"
    );
    assert_eq!(pool.in_use(), 0, "{d:?}");
  });
}

#[test]
fn invalidate_releases_the_item_instead_of_reusing_it() {
  on_both(|d| {
    let (calls, released) = (counter(), log());
    let pool: Pool<u32> = Pool::new(2, numbered(&calls)).with_release(recording(&released));
    let p = pool.clone();
    // Invalidated while checked out: released at check-in.
    let out = Effect::scoped(move |s| {
      let p2 = p.clone();
      p.get(&s)
        .flat_map(move |a| p2.invalidate(&a).map(move |()| a))
    });
    // Invalidated while idle: released at once.
    let p3 = pool.clone();
    let idle = checkout(&pool).flat_map(move |b| p3.invalidate(&b).map(move |()| b));
    let eff = out.zip(idle).zip(checkout(&pool));
    let exit = d.run(eff);
    assert!(
      matches!(exit, Exit::Success(((1, 2), 3))),
      "{d:?}: {exit:?}"
    );
    assert_eq!(entries(&released), [1, 2], "{d:?}");
    assert_eq!(pool.idle_count(), 1, "{d:?}");
  });
}

#[test]
fn idle_items_expire_on_the_executor_clock() {
  let d = Driver::Test;
  let (calls, released) = (counter(), log());
  let pool: Pool<u32> = Pool::new(1, numbered(&calls))
    .with_ttl(10 * SEC)
    .with_release(recording(&released));
  let eff = checkout(&pool) // 1, checked in at t=0
    .zip(sleep(9 * SEC).then(checkout(&pool))) // t=9: fresh, reused, checked in at t=9
    .zip(sleep(10 * SEC).then(checkout(&pool))); // t=19: 10 s idle, expired
  let exit = d.run(eff);
  assert!(matches!(exit, Exit::Success(((1, 1), 2))), "{exit:?}");
  assert_eq!(count(&calls), 2);
  assert_eq!(entries(&released), [1], "the expired item was released");
}

#[test]
fn shutdown_releases_idle_items_and_interrupts_later_checkouts() {
  on_both(|d| {
    let (calls, released) = (counter(), log());
    let pool: Pool<u32> = Pool::new(2, numbered(&calls)).with_release(recording(&released));
    let (p, p2) = (pool.clone(), pool.clone());
    // Item 1 is idle at shutdown; item 2 is checked out and is released at its check-in.
    let eff = checkout(&pool)
      .then(Effect::scoped(move |s| {
        p.get(&s)
          .zip(p.get(&s))
          .flat_map(move |held| p.shutdown().map(move |()| held))
      }))
      .zip(Effect::scoped(move |s| p2.get(&s)).exit());
    let exit = d.run(eff);
    match exit {
      Exit::Success(((1, 2), late)) => {
        assert_eq!(ExitKind::of(&late), ExitKind::Interrupt, "{d:?}");
      }
      other => panic!("{d:?}: {other:?}"),
    }
    assert!(pool.is_shutdown());
    let mut rel = entries(&released);
    rel.sort_unstable();
    assert_eq!(rel, [1, 2], "{d:?}");
    assert_eq!((pool.in_use(), pool.idle_count()), (0, 0), "{d:?}");
  });
}

#[test]
fn shutdown_interrupts_waiters() {
  on_both(|d| {
    let calls = counter();
    let pool: Pool<u32> = Pool::new(1, numbered(&calls));
    let (p, w) = (pool.clone(), pool.clone());
    let eff = Effect::scoped(move |s| {
      p.get(&s).flat_map(move |_| {
        let w2 = w.clone();
        checkout(&w).fork().flat_map(move |h| {
          let shut = w2.shutdown();
          until(move || w2.waiting() == 1)
            .then(shut)
            .then(h.join().exit())
        })
      })
    });
    let exit = d.run(eff);
    match exit {
      Exit::Success(waiter) => {
        assert_eq!(ExitKind::of(&waiter), ExitKind::Interrupt, "{d:?}")
      }
      other => panic!("{d:?}: {other:?}"),
    }
    assert_eq!((pool.in_use(), pool.waiting()), (0, 0), "{d:?}");
  });
}

// ── KeyedPool ─────────────────────────────────────────────────────────────────────────────

#[test]
fn keyed_pool_capacity_is_per_key_and_items_are_reused_per_key() {
  on_both(|d| {
    let calls = counter();
    let c = Arc::clone(&calls);
    let pool: KeyedPool<&'static str, String> = KeyedPool::new(1, move |k: &'static str| {
      let n = c.fetch_add(1, Ordering::SeqCst);
      succeed(format!("{k}-{n}"))
    });
    let (p, p2) = (pool.clone(), pool.clone());
    // Capacity 1 per key: holding "a" does not block "b".
    let both: Effect<(String, String), Never> =
      Effect::scoped(move |s| p.get("a", &s).zip(p.get("b", &s)));
    let again: Effect<String, Never> = Effect::scoped(move |s| p2.get("a", &s));
    let exit = d.run(both.zip(again));
    match exit {
      Exit::Success(((a, b), a2)) => {
        assert!(a.starts_with("a-") && b.starts_with("b-"), "{d:?}: {a} {b}");
        assert_eq!(a, a2, "{d:?}: key a reused its item");
      }
      other => panic!("{d:?}: {other:?}"),
    }
    assert_eq!(count(&calls), 2, "{d:?}");
    assert_eq!((pool.idle_count(&"a"), pool.in_use(&"a")), (1, 0), "{d:?}");
  });
}

#[test]
fn keyed_pool_item_is_checked_in_when_its_holder_is_interrupted() {
  on_both(|d| {
    let calls = counter();
    let c = Arc::clone(&calls);
    let pool: KeyedPool<u8, u32> = KeyedPool::new(1, move |k: u8| {
      let n = c.fetch_add(1, Ordering::SeqCst) as u32;
      succeed(u32::from(k) * 100 + n)
    });
    let holding = Arc::new(AtomicBool::new(false));
    let (p, p2, hf) = (pool.clone(), pool.clone(), Arc::clone(&holding));
    let holder: Effect<(), Never> =
      Effect::scoped(move |s| p.get(7, &s).then(set(&hf)).then(sleep(HOUR)));
    let eff = holder
      .fork()
      .flat_map(move |h| until(is_set(&holding)).then(h.interrupt()))
      .flat_map(move |_| Effect::scoped(move |s| p2.get(7, &s)));
    assert!(matches!(d.run(eff), Exit::Success(700)), "{d:?}");
    assert_eq!(count(&calls), 1, "{d:?}");
    assert_eq!(pool.in_use(&7), 0, "{d:?}");
  });
}

#[test]
fn keyed_pool_shutdown_interrupts_later_checkouts() {
  on_both(|d| {
    let pool: KeyedPool<u8, u8> = KeyedPool::new(1, succeed);
    let p = pool.clone();
    let eff = pool
      .shutdown()
      .then(Effect::scoped(move |s| p.get(1, &s)).exit());
    match d.run(eff) {
      Exit::Success(late) => assert_eq!(ExitKind::of(&late), ExitKind::Interrupt, "{d:?}"),
      other => panic!("{d:?}: {other:?}"),
    }
  });
}

// ── Cache ─────────────────────────────────────────────────────────────────────────────────

/// A lookup `k -> k * 10` that counts its calls.
fn times_ten(c: &Arc<AtomicUsize>) -> impl Fn(u32) -> Effect<u32, Never> + Send + Sync + 'static {
  let c = Arc::clone(c);
  move |k| {
    let c = Arc::clone(&c);
    sync(move || {
      c.fetch_add(1, Ordering::SeqCst);
      k * 10
    })
  }
}

#[test]
fn cache_constructor_is_plain_and_looks_nothing_up() {
  let calls = counter();
  let cache: Cache<u32, u32> = Cache::new(4, None, times_ten(&calls));
  let _ = cache.clone();
  assert_eq!(count(&calls), 0);
}

#[test]
fn cache_hits_after_the_first_lookup_and_counts() {
  on_both(|d| {
    let calls = counter();
    let cache: Cache<u32, u32> = Cache::new(8, None, times_ten(&calls));
    let eff = cache
      .get(1)
      .zip(cache.get(1))
      .zip(cache.get(2))
      .zip(cache.stats());
    let exit = d.run(eff);
    assert!(
      matches!(
        exit,
        Exit::Success((
          ((10, 10), 20),
          CacheStats {
            hits: 1,
            misses: 2,
            size: 2
          }
        ))
      ),
      "{d:?}: {exit:?}"
    );
    assert_eq!(count(&calls), 2, "{d:?}");
  });
}

#[test]
fn concurrent_misses_share_one_lookup() {
  on_both(|d| {
    let calls = counter();
    let c = Arc::clone(&calls);
    let cache: Cache<u32, u32> = Cache::new(8, None, move |k| {
      c.fetch_add(1, Ordering::SeqCst);
      sleep(SEC).map(move |()| k + 1)
    });
    let gets = (0..8).map(|_| cache.get(41)).collect();
    let eff = fork_all(gets, Concurrency::Unbounded).zip(cache.stats());
    match d.run(eff) {
      Exit::Success((vs, stats)) => {
        assert_eq!(vs, vec![42; 8], "{d:?}");
        assert_eq!((stats.hits, stats.misses, stats.size), (0, 8, 1), "{d:?}");
      }
      other => panic!("{d:?}: {other:?}"),
    }
    assert_eq!(count(&calls), 1, "{d:?}");
  });
}

#[test]
fn cache_entries_expire_on_the_executor_clock() {
  let d = Driver::Test;
  let calls = counter();
  let cache: Cache<u32, u32> = Cache::new(8, Some(10 * SEC), times_ten(&calls));
  let eff = cache
    .get(1) // stored at t=0
    .zip(sleep(9 * SEC).then(cache.get(1))) // t=9: fresh
    .zip(sleep(SEC).then(cache.contains(&1))) // t=10: expired
    .zip(cache.get(1)); // looked up again
  let exit = d.run(eff);
  assert!(
    matches!(exit, Exit::Success((((10, 10), false), 10))),
    "{exit:?}"
  );
  assert_eq!(count(&calls), 2);
}

#[test]
fn cache_evicts_the_least_recently_used_entry_beyond_capacity() {
  on_both(|d| {
    let calls = counter();
    let cache: Cache<u32, u32> = Cache::new(2, None, times_ten(&calls));
    // 1, 2, touch 1, 3 (evicts 2), 1 is a hit, 2 is looked up again.
    let eff = cache
      .get(1)
      .then(cache.get(2))
      .then(cache.get(1))
      .then(cache.get(3))
      .then(cache.contains(&2))
      .zip(cache.contains(&1))
      .zip(cache.get(1).then(cache.get(2)))
      .zip(cache.size());
    let exit = d.run(eff);
    assert!(
      matches!(exit, Exit::Success((((false, true), 20), 2))),
      "{d:?}: {exit:?}"
    );
    assert_eq!(count(&calls), 4, "{d:?}: 1, 2, 3, then 2 again");
  });
}

#[test]
fn typed_failures_are_cached_and_defects_are_not() {
  on_both(|d| {
    let calls = counter();
    let c = Arc::clone(&calls);
    let cache: Cache<u32, u32, String> = Cache::new(8, None, move |k| {
      c.fetch_add(1, Ordering::SeqCst);
      match k {
        0 => fail(format!("no {k}")),
        _ => die("lookup panicked"),
      }
    });
    let eff = cache
      .get(0)
      .exit::<Never>()
      .zip(cache.get(0).exit())
      .zip(cache.get(1).exit())
      .zip(cache.get(1).exit());
    let exit = d.run(eff);
    match exit {
      Exit::Success((((f1, f2), d1), d2)) => {
        assert!(
          matches!(f1, Exit::Failure(Cause::Fail(ref e)) if e == "no 0"),
          "{d:?}"
        );
        assert!(
          matches!(f2, Exit::Failure(Cause::Fail(ref e)) if e == "no 0"),
          "{d:?}"
        );
        assert_eq!(ExitKind::of(&d1), ExitKind::Die, "{d:?}");
        assert_eq!(ExitKind::of(&d2), ExitKind::Die, "{d:?}");
      }
      other => panic!("{d:?}: {other:?}"),
    }
    assert_eq!(
      count(&calls),
      3,
      "{d:?}: the failure once, the defect twice"
    );
  });
}

#[test]
fn waiters_retry_when_the_leading_lookup_is_interrupted() {
  on_both(|d| {
    let calls = counter();
    let c = Arc::clone(&calls);
    let cache: Cache<u32, u32> = Cache::new(8, None, move |k| {
      match c.fetch_add(1, Ordering::SeqCst) {
        0 => sleep(HOUR).map(move |()| k), // the leader hangs until interrupted
        _ => succeed(k * 2),
      }
    });
    let (c1, c2, c3) = (cache.clone(), cache.clone(), cache.clone());
    let calls2 = Arc::clone(&calls);
    let eff = c1.get(5).fork().flat_map(move |leader| {
      until(move || count(&calls2) == 1).then(c2.get(5).fork().flat_map(move |waiter| {
        // The waiter has registered once the miss count reaches 2.
        wait_misses(c3, 2)
          .then(leader.interrupt())
          .flat_map(move |lexit| waiter.join().map(move |v| (ExitKind::of(&lexit), v)))
      }))
    });
    let exit = d.run(eff);
    assert!(
      matches!(exit, Exit::Success((ExitKind::Interrupt, 10))),
      "{d:?}: {exit:?}"
    );
    assert_eq!(count(&calls), 2, "{d:?}");
  });
}

fn wait_misses(cache: Cache<u32, u32>, n: u64) -> Effect<(), Never> {
  cache.stats().flat_map(move |s: CacheStats| {
    if s.misses >= n {
      succeed(())
    } else {
      yield_fiber().then(wait_misses(cache, n))
    }
  })
}

#[test]
fn set_invalidate_refresh_and_get_if_present() {
  on_both(|d| {
    let calls = counter();
    let cache: Cache<u32, u32> = Cache::new(8, None, times_ten(&calls));
    let eff = cache
      .get_if_present(&1)
      .zip(cache.set(1, 7).then(cache.get(1)))
      .zip(cache.refresh(1).then(cache.get_if_present(&1)))
      .zip(cache.invalidate(&1).then(cache.contains(&1)))
      .zip(cache.get(2).then(cache.invalidate_all()).then(cache.size()));
    let exit = d.run(eff);
    assert!(
      matches!(exit, Exit::Success(((((None, 7), Some(10)), false), 0))),
      "{d:?}: {exit:?}"
    );
    assert_eq!(count(&calls), 2, "{d:?}: refresh(1) and get(2)");
  });
}

#[test]
fn resource_handles_are_send_and_sync() {
  fn assert_send_sync<T: Send + Sync>() {}
  assert_send_sync::<Pool<u32>>();
  assert_send_sync::<KeyedPool<String, u32>>();
  assert_send_sync::<Cache<String, u32, String>>();
}
