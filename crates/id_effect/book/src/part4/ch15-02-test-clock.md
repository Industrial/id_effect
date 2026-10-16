# Virtual Time with TestExecutor

## The Problem with Real Time in Tests

```rust
# use std::time::Duration;
# use id_effect::{Effect, Schedule, fail};
# fn ms(n: u64) -> Duration { Duration::from_millis(n) }
# fn flaky() -> Effect<u32, String> { fail("down".to_owned()) }
#[test]
fn retry_backs_off() {
  // With real time this takes 10 + 20 + 40 = 70 ms; with a 30 s backoff it takes minutes.
  let exit = Effect::retry(flaky, Schedule::exponential(ms(10)).intersect(Schedule::recurs(3)))
    .block_on();
}
# let exit = Effect::retry(flaky, Schedule::exponential(ms(10)).intersect(Schedule::recurs(3)))
#   .block_on();
# assert!(exit.into_result().is_err());
```

Real-time tests are slow and flaky. In `id_effect` there is no separate clock capability to fake:
every `sleep`, `timeout`, `retry` / `repeat` delay and schedule reads time from the **executor**
the fiber runs on. Run the effect on a `TestExecutor` and time becomes virtual.

## TestExecutor

`TestExecutor` is a single-threaded, deterministic `Executor` with a virtual clock. Nothing runs
until the test drives it:

| Method | Meaning |
|--------|---------|
| `TestExecutor::new()` | a new executor; the virtual clock starts at the current instant |
| `run_until_stalled()` | poll queued tasks until every task waits on a timer or a wake |
| `advance(d)` | move the clock forward by `d`, firing due sleeps in deadline order |
| `now()` (`Executor` trait) | the virtual clock |
| `pending_timers()`, `next_timer_deadline()`, `queued_tasks()` | inspection |

Start the effect as a root fiber on it with `eff.fork_root(Arc::new(exec.clone()))` (clones share
the executor) and check the `FiberHandle`:

```rust
use std::sync::Arc;
use std::time::Duration;
use id_effect::{Effect, TestExecutor, sleep};

#[test]
fn an_hour_long_sleep_takes_no_time() {
  let exec = TestExecutor::new();
  let eff: Effect<&'static str> = sleep(Duration::from_secs(3600)).map(|()| "done");
  let h = eff.fork_root(Arc::new(exec.clone()));

  exec.run_until_stalled();
  assert!(!h.is_done()); // parked on its timer

  exec.advance(Duration::from_secs(3599));
  assert!(!h.is_done());
  exec.advance(Duration::from_secs(1));
  assert!(h.is_done());
}
# let exec = TestExecutor::new();
# let eff: Effect<&'static str> = sleep(Duration::from_secs(3600)).map(|()| "done");
# let h = eff.fork_root(Arc::new(exec.clone()));
# exec.run_until_stalled();
# assert!(!h.is_done());
# exec.advance(Duration::from_secs(3599));
# assert!(!h.is_done());
# exec.advance(Duration::from_secs(1));
# assert!(h.is_done());
```

## Reading the Exit

`h.join_future()` is a future of the fiber's `Exit`; once `h.is_done()` it is ready on its first
poll. The kernel tests use a tiny helper for that, and another one that advances the clock from
deadline to deadline until the fiber finishes (from `crates/id_effect/tests/schedule.rs`):

```rust
# use std::sync::Arc;
# use std::time::Duration;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use id_effect::{Effect, Executor, Exit, TestExecutor}; // `Executor` for `exec.now()`

fn take_ready<F: Future>(fut: F) -> F::Output {
  match pin!(fut).as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("root fiber is not done"),
  }
}

/// Runs `eff` on a fresh TestExecutor; returns its exit and the virtual time it took.
fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> (Exit<A, E>, Duration) {
  let exec = TestExecutor::new();
  let start = exec.now();
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  while !h.is_done() {
    let deadline = exec.next_timer_deadline().expect("stalled with no timer pending");
    exec.advance(deadline.saturating_duration_since(exec.now()));
  }
  (take_ready(h.join_future()), exec.now() - start)
}
# let (exit, took) = run_virtual(id_effect::sleep::<(), _>(Duration::from_secs(5)).map(|()| 1));
# assert_eq!((exit, took), (Exit::succeed(1), Duration::from_secs(5)));
```

## Testing Retries and Schedules

```rust
# use std::future::Future;
# use std::pin::pin;
# use std::sync::Arc;
# use std::sync::atomic::{AtomicUsize, Ordering};
# use std::task::{Context, Poll, Waker};
# use std::time::Duration;
# use id_effect::{Cause, Effect, Executor, Exit, Schedule, TestExecutor, fail, succeed};
# fn ms(n: u64) -> Duration { Duration::from_millis(n) }
# fn flaky(n: &Arc<AtomicUsize>, fails: usize) -> impl Fn() -> Effect<usize, String> + Send + Sync + use<> {
#   let n = Arc::clone(n);
#   move || {
#     let i = n.fetch_add(1, Ordering::SeqCst);
#     if i < fails { fail(format!("boom {i}")) } else { succeed(i) }
#   }
# }
# fn take_ready<F: Future>(fut: F) -> F::Output {
#   match pin!(fut).as_mut().poll(&mut Context::from_waker(Waker::noop())) {
#     Poll::Ready(v) => v,
#     Poll::Pending => panic!("root fiber is not done"),
#   }
# }
# fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> (Exit<A, E>, Duration) {
#   let exec = TestExecutor::new();
#   let start = exec.now();
#   let h = eff.fork_root(Arc::new(exec.clone()));
#   exec.run_until_stalled();
#   while !h.is_done() {
#     let deadline = exec.next_timer_deadline().expect("stalled with no timer pending");
#     exec.advance(deadline.saturating_duration_since(exec.now()));
#   }
#   (take_ready(h.join_future()), exec.now() - start)
# }
#[test]
fn exponential_10ms_with_3_retries_takes_exactly_70ms() {
  let n = Arc::new(AtomicUsize::new(0));
  let eff = Effect::retry(
    flaky(&n, usize::MAX), // a factory: `Fn() -> Effect<usize, String>`, always failing
    Schedule::exponential(ms(10)).intersect(Schedule::recurs(3)),
  );
  let (exit, took) = run_virtual(eff);
  assert_eq!(took, ms(70));
  assert_eq!(n.load(Ordering::SeqCst), 4); // 1 attempt + 3 retries
  assert!(matches!(exit, Exit::Failure(Cause::Fail(ref e)) if e == "boom 3"));
}
# let n = Arc::new(AtomicUsize::new(0));
# let eff = Effect::retry(flaky(&n, usize::MAX), Schedule::exponential(ms(10)).intersect(Schedule::recurs(3)));
# let (exit, took) = run_virtual(eff);
# assert_eq!(took, ms(70));
# assert_eq!(n.load(Ordering::SeqCst), 4);
# assert!(matches!(exit, Exit::Failure(Cause::Fail(ref e)) if e == "boom 3"));
```

Effects are one-shot, so `Effect::retry` takes a factory that builds a fresh attempt each time.
To check timing step by step, drive the executor yourself and assert between `advance` calls:

```rust
# use std::sync::Arc;
# use std::time::Duration;
# use id_effect::{Effect, Schedule, TestExecutor, fail};
# fn ms(n: u64) -> Duration { Duration::from_millis(n) }
# let eff: Effect<u32, String> = Effect::retry(
#   || fail("boom".to_owned()),
#   Schedule::exponential(ms(10)).intersect(Schedule::recurs(3)),
# );
# let exec = TestExecutor::new();
let h = eff.fork_root(Arc::new(exec.clone()));
exec.run_until_stalled();
for step in [ms(10), ms(20), ms(40) - Duration::from_nanos(1)] {
  exec.advance(step);
}
assert!(!h.is_done()); // one nanosecond early
exec.advance(Duration::from_nanos(1));
assert!(h.is_done());
```

## Timeouts and Races

`Effect::timeout(d)` races the effect against a `sleep(d)` on the same clock, so it is
deterministic under `TestExecutor` too:

```rust
# use std::future::Future;
# use std::pin::pin;
# use std::sync::Arc;
# use std::task::{Context, Poll, Waker};
# use std::time::Duration;
# use id_effect::{Effect, Executor, Exit, TestExecutor, sleep};
# fn take_ready<F: Future>(fut: F) -> F::Output {
#   match pin!(fut).as_mut().poll(&mut Context::from_waker(Waker::noop())) {
#     Poll::Ready(v) => v,
#     Poll::Pending => panic!("root fiber is not done"),
#   }
# }
# fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> (Exit<A, E>, Duration) {
#   let exec = TestExecutor::new();
#   let start = exec.now();
#   let h = eff.fork_root(Arc::new(exec.clone()));
#   exec.run_until_stalled();
#   while !h.is_done() {
#     let deadline = exec.next_timer_deadline().expect("stalled with no timer pending");
#     exec.advance(deadline.saturating_duration_since(exec.now()));
#   }
#   (take_ready(h.join_future()), exec.now() - start)
# }
let slow: Effect<u32> = sleep(Duration::from_secs(10)).map(|()| 1);
let (exit, took) = run_virtual(slow.timeout(Duration::from_secs(1)));
assert_eq!(exit, Exit::succeed(None));
assert_eq!(took, Duration::from_secs(1));
```

## Combining Virtual Time with Fake Services

Provide test doubles first, then run on the executor; `fork_root` needs `R = HNil`:

```rust
# use std::future::Future;
# use std::pin::pin;
# use std::sync::Arc;
# use std::task::{Context, Poll, Waker};
# use std::time::Duration;
# use id_effect::{Effect, Executor, Exit, TestExecutor, caps, need};
# fn take_ready<F: Future>(fut: F) -> F::Output {
#   match pin!(fut).as_mut().poll(&mut Context::from_waker(Waker::noop())) {
#     Poll::Ready(v) => v,
#     Poll::Pending => panic!("root fiber is not done"),
#   }
# }
# fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> (Exit<A, E>, Duration) {
#   let exec = TestExecutor::new();
#   let start = exec.now();
#   let h = eff.fork_root(Arc::new(exec.clone()));
#   exec.run_until_stalled();
#   while !h.is_done() {
#     let deadline = exec.next_timer_deadline().expect("stalled with no timer pending");
#     exec.advance(deadline.saturating_duration_since(exec.now()));
#   }
#   (take_ready(h.join_future()), exec.now() - start)
# }
# struct User;
# fn user_a() -> User { User }
# fn user_b() -> User { User }
# struct FakeApi(Vec<User>);
# impl FakeApi { fn new(users: Vec<User>) -> Self { FakeApi(users) } }
# fn sync_users() -> Effect<usize, String, caps!(FakeApi)> { need::<FakeApi>().map(|api| api.0.len()) }
let eff = sync_users().provide(FakeApi::new(vec![user_a(), user_b()]));
let (exit, _) = run_virtual(eff);
assert_eq!(exit, Exit::succeed(2));
```

`TestExecutor` is single-threaded, so ordering is deterministic: tasks run in FIFO order and
sleeps fire in `(deadline, registration order)`. STM retries, `Deferred` waits and queue takes
park on wakers rather than spinning, so they make progress on it as well.
