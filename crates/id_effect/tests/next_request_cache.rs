//! `p-request-cache` acceptance tests for `id_effect::next::RequestCache` (Effect.ts
//! `Request.makeCache`, `Effect.withRequestCache`, `Effect.withRequestCaching`,
//! `Layer.setRequestCache`).
//!
//! AC: with caching scoped to a `Layer` or a `Scope`, the same request made twice makes one
//! resolver call; with caching off (the default), two.

use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::next::{
  Concurrency, Effect, Executor, HNil, Layer, Request, RequestCache, RequestResolver, TestExecutor,
  TokioExecutor, all, sleep, sync,
};
use id_effect::{Cause, Exit};

// ── drivers ───────────────────────────────────────────────────────────────────────────────

fn take_ready<F: Future>(fut: F) -> F::Output {
  let mut fut = pin!(fut);
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("root fiber is not done"),
  }
}

/// Runs `eff` as a root fiber on a fresh `TestExecutor`, advancing virtual time as needed.
fn run_test<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
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

fn run_tokio<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
  static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
  let rt = RT.get_or_init(|| {
    tokio::runtime::Builder::new_multi_thread()
      .worker_threads(4)
      .enable_time()
      .build()
      .expect("tokio runtime")
  });
  let h = eff.fork_root(Arc::new(TokioExecutor::new(rt.handle().clone())));
  rt.block_on(async {
    tokio::time::timeout(Duration::from_secs(30), h.join_future())
      .await
      .expect("root fiber timed out on tokio")
  })
}

// ── fixtures ──────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct GetUser(u32);

#[derive(Clone, Debug, PartialEq, Eq)]
struct NotFound(u32);

impl Request for GetUser {
  type Output = String;
  type Error = NotFound;
}

/// Every resolver call, as the list of requested ids.
type Calls = Arc<Mutex<Vec<Vec<u32>>>>;

fn calls(c: &Calls) -> Vec<Vec<u32>> {
  c.lock().unwrap().clone()
}

/// Id `0` is not found; every other id resolves to `user-<id>`. Each call takes `delay`.
fn users_with_delay(delay: Duration) -> (RequestResolver<GetUser>, Calls) {
  let log: Calls = Arc::default();
  let seen = Arc::clone(&log);
  let resolver = RequestResolver::make_batched(move |reqs: Vec<GetUser>| {
    let seen = Arc::clone(&seen);
    sleep(delay).flat_map(move |()| {
      sync(move || {
        seen
          .lock()
          .unwrap()
          .push(reqs.iter().map(|r| r.0).collect());
        reqs
          .into_iter()
          .map(|GetUser(id)| {
            if id == 0 {
              Err(NotFound(id))
            } else {
              Ok(format!("user-{id}"))
            }
          })
          .collect()
      })
    })
  });
  (resolver, log)
}

fn users() -> (RequestResolver<GetUser>, Calls) {
  users_with_delay(Duration::ZERO)
}

fn get(id: u32, r: &RequestResolver<GetUser>) -> Effect<String, NotFound> {
  Effect::request(GetUser(id), r)
}

/// The same request made twice, one after the other (two batch windows).
fn twice(id: u32, r: &RequestResolver<GetUser>) -> Effect<(String, String), NotFound> {
  let r2 = r.clone();
  get(id, r).flat_map(move |a| get(id, &r2).map(move |b| (a, b)))
}

fn pair(id: u32) -> (String, String) {
  (format!("user-{id}"), format!("user-{id}"))
}

// ── AC: caching off (the default) → 2 calls ───────────────────────────────────────────────

#[test]
fn caching_is_off_by_default() {
  let (r, log) = users();
  assert!(matches!(run_test(twice(1, &r)), Exit::Success(v) if v == pair(1)));
  assert_eq!(
    calls(&log),
    [vec![1], vec![1]],
    "no cache: one call per request"
  );
}

#[test]
fn with_request_caching_false_turns_a_provided_cache_off() {
  let (r, log) = users();
  let cache = RequestCache::new();
  let eff = twice(1, &r)
    .with_request_caching(false)
    .with_request_cache(&cache);
  assert!(matches!(run_test(eff), Exit::Success(v) if v == pair(1)));
  assert_eq!(calls(&log), [vec![1], vec![1]]);
  assert!(cache.is_empty());
}

// ── AC: caching scoped to a Layer → 1 call ────────────────────────────────────────────────

#[test]
fn layer_scoped_cache_makes_one_call() {
  let (r, log) = users();
  let cache = RequestCache::new();
  let eff = twice(1, &r).provide_layer(cache.layer());
  assert!(matches!(run_test(eff), Exit::Success(v) if v == pair(1)));
  assert_eq!(
    calls(&log),
    [vec![1]],
    "second request answered from the cache"
  );
  assert_eq!(cache.len(), 1);
}

#[test]
fn layer_cache_merges_into_an_application_layer() {
  struct Greeting;
  let (r, log) = users();
  let cache = RequestCache::new();
  let app = Layer::<_, NotFound>::succeed(id_effect::next::hlist![Greeting]).merge(cache.layer());
  // The application layer's output holds more than the effect needs: extras are allowed.
  let eff = twice(1, &r).provide_layer(app);
  assert!(matches!(run_test(eff), Exit::Success(v) if v == pair(1)));
  assert_eq!(calls(&log), [vec![1]]);
}

#[test]
fn layer_cache_is_restored_when_the_build_scope_closes() {
  let (r, log) = users();
  let cache = RequestCache::new();
  let r2 = r.clone();
  // Inside provide_layer: cached. After it: caching off again, so two more calls.
  let eff = twice(1, &r)
    .provide_layer(cache.layer())
    .flat_map(move |_| twice(1, &r2));
  assert!(matches!(run_test(eff), Exit::Success(v) if v == pair(1)));
  assert_eq!(calls(&log), [vec![1], vec![1], vec![1]]);
}

// ── AC: caching scoped to a Scope → 1 call ────────────────────────────────────────────────

#[test]
fn scope_scoped_cache_makes_one_call_and_is_restored_on_close() {
  let (r, log) = users();
  let cache = RequestCache::new();
  let (c2, r2, r3) = (cache.clone(), r.clone(), r.clone());
  let eff = Effect::scoped(move |scope| {
    c2.provide_scoped(&scope)
      .flat_map(move |()| twice(1, &r2))
      .flat_map(|v| RequestCache::current().map(move |c| (v, c.is_some())))
  })
  .flat_map(move |inside| {
    RequestCache::current()
      .flat_map(move |after| twice(1, &r3).map(move |_| (inside, after.is_none())))
  });
  let Exit::Success(((v, on_inside), off_after)) = run_test(eff) else {
    panic!("the effect failed")
  };
  assert_eq!(v, pair(1));
  assert!(on_inside, "caching is on until the scope closes");
  assert!(off_after, "closing the scope restores the previous setting");
  // One call inside the scope, two after it.
  assert_eq!(calls(&log), [vec![1], vec![1], vec![1]]);
}

#[test]
fn with_request_cache_makes_one_call() {
  let (r, log) = users();
  let cache = RequestCache::new();
  assert!(matches!(
    run_test(twice(1, &r).with_request_cache(&cache)),
    Exit::Success(v) if v == pair(1)
  ));
  assert_eq!(calls(&log), [vec![1]]);
}

#[test]
fn with_request_caching_true_uses_the_default_cache() {
  // A fresh resolver: its keys cannot collide with other tests in the process-wide cache.
  let (r, log) = users();
  assert!(matches!(
    run_test(twice(7, &r).with_request_caching(true)),
    Exit::Success(v) if v == pair(7)
  ));
  assert_eq!(calls(&log), [vec![7]]);
}

// ── semantics ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_cache_outlives_one_run() {
  let (r, log) = users();
  let cache = RequestCache::new();
  for _ in 0..3 {
    assert!(matches!(
      run_test(get(2, &r).with_request_cache(&cache)),
      Exit::Success(s) if s == "user-2"
    ));
  }
  assert_eq!(calls(&log), [vec![2]]);
}

#[test]
fn failures_are_cached() {
  // Ruling: Effect.ts caches the request's Exit, so a typed failure is answered from the cache.
  let (r, log) = users();
  let cache = RequestCache::new();
  let r2 = r.clone();
  let eff = get(0, &r)
    .exit()
    .flat_map(move |first| get(0, &r2).exit().map(move |second| (first, second)))
    .with_request_cache(&cache);
  let Exit::Success((first, second)) = run_test::<_, NotFound>(eff) else {
    panic!("exit() does not fail")
  };
  for e in [first, second] {
    assert!(matches!(e, Exit::Failure(Cause::Fail(NotFound(0)))));
  }
  assert_eq!(calls(&log), [vec![0]]);
}

#[test]
fn an_in_flight_request_is_shared() {
  // The first request's resolver call takes 5s; the second request (same key, another window,
  // made after 1s) waits on the cached in-flight result.
  let (r, log) = users_with_delay(Duration::from_secs(5));
  let r2 = r.clone();
  let late = sleep(Duration::from_secs(1)).flat_map(move |()| get(3, &r2));
  let cache = RequestCache::new();
  let eff = all([get(3, &r), late], Concurrency::Unbounded).with_request_cache(&cache);
  assert!(matches!(run_test(eff), Exit::Success(v) if v == ["user-3", "user-3"]));
  assert_eq!(calls(&log), [vec![3]]);

  // Without a cache the late request makes its own call.
  let (r, log) = users_with_delay(Duration::from_secs(5));
  let r2 = r.clone();
  let late = sleep(Duration::from_secs(1)).flat_map(move |()| get(3, &r2));
  let eff = all([get(3, &r), late], Concurrency::Unbounded);
  assert!(matches!(run_test(eff), Exit::Success(_)));
  assert_eq!(calls(&log), [vec![3], vec![3]]);
}

#[test]
fn forked_children_inherit_the_cache() {
  let (r, log) = users();
  let cache = RequestCache::new();
  let r2 = r.clone();
  let eff = get(4, &r)
    .flat_map(move |_| all([get(4, &r2), get(5, &r2)], Concurrency::Unbounded))
    .with_request_cache(&cache);
  assert!(matches!(run_test(eff), Exit::Success(v) if v == ["user-4", "user-5"]));
  assert_eq!(
    calls(&log),
    [vec![4], vec![5]],
    "4 answered from the cache in the child"
  );
}

#[test]
fn keys_are_per_resolver() {
  let (a, log_a) = users();
  let (b, log_b) = users();
  let cache = RequestCache::new();
  let b2 = b.clone();
  let eff = get(1, &a)
    .flat_map(move |_| get(1, &b2))
    .with_request_cache(&cache);
  assert!(matches!(run_test(eff), Exit::Success(_)));
  assert_eq!(
    (calls(&log_a), calls(&log_b)),
    (vec![vec![1]], vec![vec![1]])
  );
  assert_eq!(cache.len(), 2);
}

#[test]
fn capacity_evicts_the_oldest_entry() {
  let (r, log) = users();
  let cache = RequestCache::with_capacity(1);
  let (r2, r3) = (r.clone(), r.clone());
  let eff = get(1, &r)
    .flat_map(move |_| get(2, &r2))
    .flat_map(move |_| get(1, &r3))
    .with_request_cache(&cache);
  assert!(matches!(run_test(eff), Exit::Success(s) if s == "user-1"));
  assert_eq!(calls(&log), [vec![1], vec![2], vec![1]]);
  assert_eq!((cache.len(), cache.capacity()), (1, 1));
}

#[test]
fn clear_forgets_entries() {
  let (r, log) = users();
  let cache = RequestCache::new();
  assert!(matches!(
    run_test(get(1, &r).with_request_cache(&cache)),
    Exit::Success(_)
  ));
  cache.clear();
  assert!(cache.is_empty());
  assert!(matches!(
    run_test(get(1, &r).with_request_cache(&cache)),
    Exit::Success(_)
  ));
  assert_eq!(calls(&log), [vec![1], vec![1]]);
}

#[test]
fn concurrent_callers_on_tokio_share_one_call() {
  let (r, log) = users();
  let cache = RequestCache::new();
  let effs: Vec<_> = (0..16).map(|_| twice(9, &r)).collect();
  let eff = all(effs, Concurrency::Unbounded).with_request_cache(&cache);
  let Exit::Success(v) = run_tokio(eff) else {
    panic!("the effect failed")
  };
  assert!(v.iter().all(|p| *p == pair(9)));
  assert_eq!(
    calls(&log),
    [vec![9]],
    "the cache lock makes misses race-free"
  );
}

#[test]
fn layer_type_is_a_closed_hnil_layer() {
  let layer: Layer<HNil, NotFound> = RequestCache::new().layer();
  assert!(matches!(run_test(layer.build()), Exit::Success(HNil)));
}
