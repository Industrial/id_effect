//! `p-request-batch` acceptance tests for `id_effect::{Effect::request, RequestResolver}`.
//!
//! The batching assertions run on the deterministic `TestExecutor` (FIFO run queue, driven with
//! `run_until_stalled`), where the children of one `all` always share a batch window. Results
//! (which never depend on batching) are also checked on a multi-thread `TokioExecutor`.

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::Never;
use id_effect::{Cause, Exit};
use id_effect::{
  Concurrency, Effect, Executor, Request, RequestResolver, TestExecutor, TokioExecutor, all, fail,
  request, sleep, succeed, sync,
};

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

/// Runs `eff` as a root fiber on the multi-thread tokio runtime.
fn run_tokio<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
  let rt = tokio_rt();
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
enum UserError {
  /// Per-request failure: the user does not exist.
  NotFound(u32),
  /// Whole-batch failure: the backend is down.
  Down,
}

impl Request for GetUser {
  type Output = String;
  type Error = UserError;
}

/// Every resolver call, as the list of requested ids.
type Calls = Arc<Mutex<Vec<Vec<u32>>>>;

fn calls(c: &Calls) -> Vec<Vec<u32>> {
  c.lock().unwrap().clone()
}

/// Ids `0` are not found; every other id resolves to `user-<id>`.
fn users() -> (RequestResolver<GetUser>, Calls) {
  let log: Calls = Arc::default();
  let seen = Arc::clone(&log);
  let resolver = RequestResolver::make_batched(move |reqs: Vec<GetUser>| {
    let seen = Arc::clone(&seen);
    sync(move || {
      seen
        .lock()
        .unwrap()
        .push(reqs.iter().map(|r| r.0).collect());
      reqs
        .into_iter()
        .map(|GetUser(id)| {
          if id == 0 {
            Err(UserError::NotFound(id))
          } else {
            Ok(format!("user-{id}"))
          }
        })
        .collect()
    })
  });
  (resolver, log)
}

fn get(id: u32, r: &RequestResolver<GetUser>) -> Effect<String, UserError> {
  Effect::request(GetUser(id), r)
}

/// `true` when every leaf of `c` satisfies `leaf` (the requests of one batch all fail at once,
/// so `all` combines their genuine failures with `Cause::Both`).
fn every_leaf<E>(c: &Cause<E>, leaf: &impl Fn(&Cause<E>) -> bool) -> bool {
  match c {
    Cause::Both(l, r) | Cause::Then(l, r) => every_leaf(l, leaf) && every_leaf(r, leaf),
    c => leaf(c),
  }
}

fn names(ids: &[u32]) -> Vec<String> {
  ids.iter().map(|id| format!("user-{id}")).collect()
}

// ── AC: all([r1, r2, r1]) makes 1 resolver call with keys {1, 2} ─────────────────────────

#[test]
fn all_dedups_and_batches_into_one_call() {
  let (r, log) = users();
  let eff = all([get(1, &r), get(2, &r), get(1, &r)], Concurrency::Unbounded);
  assert!(matches!(run_test(eff), Exit::Success(v) if v == names(&[1, 2, 1])));
  assert_eq!(calls(&log), [vec![1, 2]], "one call, keys deduplicated");
}

#[test]
fn bounded_all_batches_each_started_group() {
  // `Bounded(2)` starts two children, the next two only as the first ones finish: every group
  // that starts together is one call.
  let (r, log) = users();
  let eff = all(
    [get(1, &r), get(2, &r), get(3, &r), get(4, &r)],
    Concurrency::Bounded(2),
  );
  assert!(matches!(run_test(eff), Exit::Success(v) if v == names(&[1, 2, 3, 4])));
  assert_eq!(calls(&log), [vec![1, 2], vec![3, 4]]);
}

#[test]
fn nested_all_and_zip_share_one_window() {
  let (r, log) = users();
  let inner = all([get(3, &r), get(1, &r)], Concurrency::Unbounded);
  let eff = all(
    [inner, all([get(2, &r)], Concurrency::Unbounded)],
    Concurrency::Unbounded,
  );
  let exit = run_test(eff);
  assert!(matches!(exit, Exit::Success(v) if v == [names(&[3, 1]), names(&[2])]));
  let log = calls(&log);
  assert_eq!(log.len(), 1, "nested siblings still batch: {log:?}");
  let mut keys = log[0].clone();
  keys.sort_unstable();
  assert_eq!(keys, [1, 2, 3]);
}

#[test]
fn all_batches_on_tokio_too() {
  // Results are exact on a multi-thread executor; the grouping is best-effort there, but the
  // keys {1, 2} are each resolved at least once and dedup never makes more calls than keys.
  for _ in 0..50 {
    let (r, log) = users();
    let eff = all([get(1, &r), get(2, &r), get(1, &r)], Concurrency::Unbounded);
    assert!(matches!(run_tokio(eff), Exit::Success(v) if v == names(&[1, 2, 1])));
    let mut keys: Vec<u32> = calls(&log).concat();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys, [1, 2]);
    assert!(calls(&log).len() <= 3);
  }
}

// ── AC: sequential requests make 1 call each ──────────────────────────────────────────────

#[test]
fn sequential_requests_make_one_call_each() {
  let (r, log) = users();
  let r2 = r.clone();
  let r3 = r.clone();
  let eff = get(1, &r)
    .flat_map(move |a| get(2, &r2).map(move |b| (a, b)))
    .flat_map(move |(a, b)| get(1, &r3).map(move |c| vec![a, b, c]));
  assert!(matches!(run_test(eff), Exit::Success(v) if v == names(&[1, 2, 1])));
  assert_eq!(calls(&log), [vec![1], vec![2], vec![1]]);
}

#[test]
fn sequential_all_makes_one_call_each() {
  let (r, log) = users();
  let eff = all(
    [get(1, &r), get(2, &r), get(1, &r)],
    Concurrency::Sequential,
  );
  assert!(matches!(run_test(eff), Exit::Success(v) if v == names(&[1, 2, 1])));
  assert_eq!(calls(&log), [vec![1], vec![2], vec![1]]);
}

#[test]
fn a_late_sibling_opens_a_new_window() {
  // The second child sleeps before its request: the first window closes without it.
  let (r, log) = users();
  let r2 = r.clone();
  let late = sleep(Duration::from_secs(1)).flat_map(move |()| get(2, &r2));
  let eff = all([get(1, &r), late], Concurrency::Unbounded);
  assert!(matches!(run_test(eff), Exit::Success(v) if v == names(&[1, 2])));
  assert_eq!(calls(&log), [vec![1], vec![2]]);
}

#[test]
fn distinct_resolvers_do_not_share_a_window() {
  let (a, log_a) = users();
  let (b, log_b) = users();
  assert_ne!(a.id(), b.id());
  assert_eq!(a.id(), a.clone().id(), "clones share the resolver identity");
  let eff = all([get(1, &a), get(1, &b), get(2, &a)], Concurrency::Unbounded);
  assert!(matches!(run_test(eff), Exit::Success(v) if v == names(&[1, 1, 2])));
  assert_eq!(calls(&log_a), [vec![1, 2]]);
  assert_eq!(calls(&log_b), [vec![1]]);
}

// ── AC: resolver errors are typed ─────────────────────────────────────────────────────────

#[test]
fn a_per_request_error_is_a_typed_failure() {
  let (r, log) = users();
  let eff = all(
    [
      get(1, &r).exit::<Never>(),
      get(0, &r).exit(),
      get(2, &r).exit(),
    ],
    Concurrency::Unbounded,
  );
  let Exit::Success(exits) = run_test(eff) else {
    panic!("exits are observed as values");
  };
  assert!(matches!(&exits[0], Exit::Success(s) if s == "user-1"));
  assert!(matches!(
    &exits[1],
    Exit::Failure(Cause::Fail(UserError::NotFound(0)))
  ));
  assert!(matches!(&exits[2], Exit::Success(s) if s == "user-2"));
  assert_eq!(calls(&log), [vec![1, 0, 2]]);
}

#[test]
fn a_failed_resolver_fails_every_request_with_its_typed_error() {
  let hits = Arc::new(AtomicUsize::new(0));
  let h = Arc::clone(&hits);
  let down = RequestResolver::make_batched(move |_reqs: Vec<GetUser>| {
    h.fetch_add(1, Ordering::SeqCst);
    fail(UserError::Down)
  });
  let eff = all(
    [
      Effect::request(GetUser(1), &down).exit::<Never>(),
      Effect::request(GetUser(2), &down).exit(),
    ],
    Concurrency::Unbounded,
  );
  let Exit::Success(exits) = run_test(eff) else {
    panic!("exits are observed as values");
  };
  for e in &exits {
    assert!(
      matches!(e, Exit::Failure(Cause::Fail(UserError::Down))),
      "{e:?}"
    );
  }
  assert_eq!(hits.load(Ordering::SeqCst), 1);
  // And as the result of `all` itself: fail-fast with the typed error.
  let exit = run_test(all(
    [request(GetUser(1), &down), request(GetUser(2), &down)],
    Concurrency::Unbounded,
  ));
  let Exit::Failure(c) = exit else {
    panic!("the batch failed");
  };
  assert!(
    every_leaf(&c, &|c| matches!(c, Cause::Fail(UserError::Down))),
    "{c:?}"
  );
}

#[test]
fn a_typed_error_reaches_tokio_requesters() {
  let (r, _) = users();
  let exit = run_tokio(get(0, &r));
  assert!(matches!(
    exit,
    Exit::Failure(Cause::Fail(UserError::NotFound(0)))
  ));
}

#[test]
fn a_short_result_or_a_panicking_resolver_is_a_defect() {
  let short = RequestResolver::make_batched(|_reqs: Vec<GetUser>| succeed(vec![]));
  let exit = run_test(all(
    [request(GetUser(1), &short), request(GetUser(2), &short)],
    Concurrency::Unbounded,
  ));
  let Exit::Failure(c) = exit else {
    panic!("a short result is a defect");
  };
  assert!(every_leaf(&c, &|c| matches!(c, Cause::Die(_))), "{c:?}");

  let panics = RequestResolver::make_batched(|_reqs: Vec<GetUser>| -> Effect<_, UserError> {
    sync(|| panic!("resolver bug"))
  });
  let exit = run_test(request(GetUser(1), &panics));
  assert!(matches!(exit, Exit::Failure(Cause::Die(_))), "{exit:?}");
}

#[test]
fn an_interrupted_requester_does_not_break_its_window() {
  // The first child fails at once and interrupts its sibling, which is waiting on a request
  // whose window the third child shares; the window is still flushed and nothing hangs.
  let (r, log) = users();
  let eff = all(
    [fail(UserError::Down), get(1, &r), get(2, &r)],
    Concurrency::Unbounded,
  );
  assert!(matches!(
    run_test(eff),
    Exit::Failure(Cause::Fail(UserError::Down))
  ));
  // A later request still resolves through the same resolver.
  assert!(matches!(run_test(get(3, &r)), Exit::Success(s) if s == "user-3"));
  assert!(calls(&log).contains(&vec![3]));
}

// ── the environment ───────────────────────────────────────────────────────────────────────

#[test]
fn the_free_fn_and_the_associated_fn_agree() {
  let (r, log) = users();
  let eff = zip_like(request(GetUser(7), &r), Effect::request(GetUser(7), &r));
  assert!(matches!(run_test(eff), Exit::Success((a, b)) if a == "user-7" && b == "user-7"));
  assert_eq!(calls(&log), [vec![7]]);
}

fn zip_like(
  a: Effect<String, UserError>,
  b: Effect<String, UserError>,
) -> Effect<(String, String), UserError> {
  id_effect::zip_par(a, b)
}

#[test]
fn batching_needs_no_timer() {
  // The flush fiber runs on the requester's executor (here the test executor's virtual clock
  // never moves: batching needs no timer).
  let exec = TestExecutor::new();
  let t0 = exec.now();
  let (r, _) = users();
  let h = all([get(1, &r), get(2, &r)], Concurrency::Unbounded).fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  assert!(h.is_done());
  assert_eq!(exec.now(), t0);
}
