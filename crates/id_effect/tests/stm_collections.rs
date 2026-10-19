//! `conc.stm#6` acceptance: `TSet`, `TArray`, `TPriorityQueue`, `TPromise` and `TReentrantLock`.
//!
//! Each collection is checked for atomicity with other cells, for composing with `Stm::or_else`
//! and `Stm::retry`, and (where an operation waits) for parking the fiber until a commit wakes
//! it. The deterministic tests run on the single-threaded `TestExecutor`, where a spinning retry
//! would never stall; the contention test runs on a multi-thread tokio runtime.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::mpsc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::stm::{Stm, TArray, TPriorityQueue, TPromise, TReentrantLock, TRef, TSet, commit};
use id_effect::{Cause, Effect, Exit, FiberHandle, HNil, Runtime, TestExecutor, succeed, suspend};

/// Commits `stm` on the global runtime; typed failures become `Err`.
fn run<A: Send + 'static, E: Send + 'static>(stm: Stm<A, E>) -> Result<A, E> {
  match commit(stm).block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("transaction died or was interrupted"),
  }
}

/// Commits a constructor (error type `()`) and returns the handle.
fn make<A: Send + 'static>(stm: Stm<A, ()>) -> A {
  run(stm).expect("constructor")
}

/// Forks `eff` as a root fiber on `exec` and runs the executor until it stalls.
fn spawn<A: Send + 'static, E: Send + 'static>(
  exec: &TestExecutor,
  eff: Effect<A, E>,
) -> FiberHandle<A, E> {
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  h
}

/// The exit of a finished fiber.
fn exit_of<A: Send + 'static, E: Send + 'static>(h: FiberHandle<A, E>) -> Exit<A, E> {
  assert!(h.is_done(), "fiber has not finished");
  let mut fut = pin!(h.join_future());
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("join of a finished fiber is not ready"),
  }
}

// ── TSet ─────────────────────────────────────────────────────────────────────

#[test]
fn tset_add_remove_atomic_with_tref() {
  let set = make(TSet::<u32>::make());
  let count = make(TRef::make(0u32));

  // add + counter bump commit together.
  let add_and_count = {
    let count = count.clone();
    set
      .add::<&str>(1)
      .flat_map(move |added| count.update_stm(move |n| if added { n + 1 } else { n }))
  };
  assert_eq!(run(add_and_count.clone()), Ok(()));
  assert_eq!(run(add_and_count), Ok(()), "a second add of 1 is a no-op");
  assert_eq!(run(count.read_stm::<()>()), Ok(1));

  // A transaction that removes, bumps the counter, then fails commits neither.
  let failing = {
    let count = count.clone();
    set
      .remove::<&str>(&1)
      .flat_map(move |_| count.write_stm(99))
      .flat_map(|()| Stm::<(), &str>::fail("abort"))
  };
  assert_eq!(run(failing), Err("abort"));
  assert_eq!(run(set.contains::<()>(&1)), Ok(true));
  assert_eq!(run(count.read_stm::<()>()), Ok(1));

  // `or_else` rolls back a retrying branch's add; `take_any` retries on an empty set.
  let empty = make(TSet::<u32>::make());
  let branch = {
    let empty = empty.clone();
    set
      .add::<()>(2)
      .flat_map(move |_| empty.take_any())
      .or_else(Stm::succeed(0))
  };
  assert_eq!(run(branch), Ok(0));
  assert_eq!(
    run(set.contains::<()>(&2)),
    Ok(false),
    "rolled back by or_else"
  );
  assert_eq!(run(set.size::<()>()), Ok(1));
  assert_eq!(run(set.remove::<()>(&1)), Ok(true));
  assert_eq!(run(set.is_empty::<()>()), Ok(true));
}

#[test]
fn tset_take_any_parks_until_add() {
  let exec = TestExecutor::new();
  let set = make(TSet::<u32>::make());
  let taker = spawn(&exec, commit(set.take_any::<()>()));
  assert!(!taker.is_done(), "parked while empty");
  spawn(&exec, commit(set.add::<()>(5)));
  assert_eq!(exit_of(taker), Exit::succeed(5));
  assert_eq!(run(set.to_vec::<()>()), Ok(vec![]));
}

// ── TArray ───────────────────────────────────────────────────────────────────

#[test]
fn tarray_update_is_transactional() {
  let arr = make(TArray::make(vec![1u32, 2, 3]));
  assert_eq!(arr.len(), 3);

  // Two element updates commit together.
  let both = {
    let arr2 = arr.clone();
    arr
      .update::<(), _>(0, |n| n + 10)
      .flat_map(move |_| arr2.update(2, |n| n * 10))
  };
  assert_eq!(run(both), Ok(true));
  assert_eq!(run(arr.to_vec::<()>()), Ok(vec![11, 2, 30]));

  // A failing transaction leaves every element unchanged.
  let failing = arr
    .set::<&str>(1, 0)
    .flat_map(|_| Stm::<bool, &str>::fail("abort"));
  assert_eq!(run(failing), Err("abort"));
  assert_eq!(run(arr.get::<()>(1)), Ok(Some(2)));

  // Out of range is not a failure.
  assert_eq!(run(arr.get::<()>(9)), Ok(None));
  assert_eq!(run(arr.set::<()>(9, 0)), Ok(false));
  assert_eq!(run(arr.fold::<u32, (), _>(0, |a, b| a + b)), Ok(43));

  // `or_else` rolls back a retrying branch's element write.
  let branch = arr
    .set::<()>(0, 0)
    .flat_map(|_| Stm::<bool, ()>::retry())
    .or_else(Stm::succeed(false));
  assert_eq!(run(branch), Ok(false));
  assert_eq!(run(arr.get::<()>(0)), Ok(Some(11)));
}

#[test]
fn tarray_retry_parks_on_the_elements_it_read() {
  let exec = TestExecutor::new();
  let arr = make(TArray::make(vec![0u32, 0]));
  let wait_first = {
    let arr = arr.clone();
    arr
      .get::<()>(0)
      .flat_map(|v| Stm::<(), ()>::check(v != Some(0)).map(move |()| v))
  };
  let h = spawn(&exec, commit(wait_first));
  assert!(!h.is_done());
  spawn(&exec, commit(arr.set::<()>(1, 7)));
  assert!(!h.is_done(), "a write to another element does not wake it");
  spawn(&exec, commit(arr.set::<()>(0, 4)));
  assert_eq!(exit_of(h), Exit::succeed(Some(4)));
}

// ── TPriorityQueue ──────────────────────────────────────────────────────────

#[test]
fn tpriority_queue_orders_and_retries_when_empty() {
  let exec = TestExecutor::new();
  let q = make(TPriorityQueue::<u32>::empty());

  let taker = spawn(&exec, commit(q.take::<()>()));
  assert!(!taker.is_done(), "take parks while empty");
  spawn(&exec, commit(q.offer_all::<()>(vec![5, 1, 4])));
  assert_eq!(exit_of(taker), Exit::succeed(1), "smallest first");

  assert_eq!(run(q.peek::<()>()), Ok(4));
  assert_eq!(run(q.offer::<()>(2)), Ok(()));
  assert_eq!(run(q.size::<()>()), Ok(3));
  assert_eq!(run(q.take_all::<()>()), Ok(vec![2, 4, 5]));
  assert_eq!(run(q.take_option::<()>()), Ok(None));
  assert_eq!(run(q.is_empty::<()>()), Ok(true));

  // `or_else` falls back when the queue would retry, and keeps no partial writes.
  let other = make(TPriorityQueue::<u32>::from_values([9]));
  let pick = {
    let other = other.clone();
    q.take::<()>().or_else(other.take())
  };
  assert_eq!(run(pick), Ok(9));
  assert_eq!(run(other.is_empty::<()>()), Ok(true));
}

// ── TPromise ─────────────────────────────────────────────────────────────────

#[test]
fn tpromise_await_parks_until_completed() {
  let exec = TestExecutor::new();
  let p = make(TPromise::<u32, String>::make());

  let waiter = spawn(&exec, commit(p.await_()));
  assert!(!waiter.is_done(), "await_ parks while pending");
  assert_eq!(run(p.is_done::<()>()), Ok(false));

  let completer = spawn(&exec, commit(p.succeed::<()>(42)));
  assert_eq!(exit_of(completer), Exit::succeed(true));
  assert_eq!(exit_of(waiter), Exit::succeed(42));

  // Completes once: later completions are ignored.
  assert_eq!(run(p.succeed::<()>(7)), Ok(false));
  assert_eq!(run(p.fail::<()>("late".into())), Ok(false));
  assert_eq!(run(p.await_()), Ok(42));
  assert_eq!(run(p.poll::<()>()), Ok(Some(Ok(42))));

  // A failed promise fails the awaiting transaction with its error.
  let failed = make(TPromise::<u32, String>::make());
  assert_eq!(run(failed.fail::<()>("boom".into())), Ok(true));
  assert_eq!(run(failed.await_()), Err("boom".to_string()));

  // `or_else` gives a default instead of parking on a pending promise.
  let pending = make(TPromise::<u32, String>::make());
  assert_eq!(run(pending.await_().or_else(Stm::succeed(0))), Ok(0));
}

// ── TReentrantLock ──────────────────────────────────────────────────────────

/// Waits (STM retry) until `gate` is open.
fn wait_open(gate: &TRef<bool>) -> Stm<(), ()> {
  gate.read_stm::<()>().flat_map(Stm::<(), ()>::check)
}

#[test]
fn treentrant_lock_writer_excludes_readers() {
  let exec = TestExecutor::new();
  let lock = make(TReentrantLock::make());
  let gate = make(TRef::make(false));

  // Writer: takes the write lock twice (reentrant), holds it until the gate opens, then
  // releases both holds.
  let writer = {
    let (l1, l2, l3, l4) = (lock.clone(), lock.clone(), lock.clone(), lock.clone());
    let gate = gate.clone();
    commit(l1.acquire_write::<()>())
      .flat_map(move |_| commit(l2.acquire_write::<()>()))
      .flat_map(move |held| commit(wait_open(&gate)).map(move |()| held))
      .flat_map(move |held| commit(l3.release_write::<()>()).map(move |_| held))
      .flat_map(move |held| commit(l4.release_write::<()>()).map(move |_| held))
  };
  let w = spawn(&exec, writer);
  assert!(!w.is_done(), "writer holds the lock, waiting on the gate");
  assert_eq!(run(lock.write_locks::<()>()), Ok(2));

  // Readers from other fibers park while the writer holds the lock.
  let r1 = spawn(&exec, commit(lock.acquire_read::<()>()));
  let r2 = spawn(&exec, commit(lock.acquire_read::<()>()));
  assert!(
    !r1.is_done() && !r2.is_done(),
    "readers excluded by the writer"
  );
  assert_eq!(run(lock.read_locks::<()>()), Ok(0));

  // `or_else` turns the blocked acquire into a try-lock.
  let try_read = lock
    .acquire_read::<()>()
    .map(|_| true)
    .or_else(Stm::succeed(false));
  assert_eq!(run(try_read), Ok(false));

  // Opening the gate releases the write lock; both readers get in.
  spawn(&exec, commit(gate.write_stm::<()>(true)));
  assert_eq!(exit_of(w), Exit::succeed(2), "write lock was reentrant");
  assert_eq!(exit_of(r1), Exit::succeed(1));
  assert_eq!(exit_of(r2), Exit::succeed(1));
  assert_eq!(run(lock.read_locks::<()>()), Ok(2));
  assert_eq!(run(lock.is_write_locked::<()>()), Ok(false));

  // A writer from another fiber now parks behind the two readers.
  let w2 = spawn(&exec, commit(lock.acquire_write::<()>()));
  assert!(!w2.is_done(), "writer excluded by readers of other fibers");
  // Releasing from a fiber that holds nothing changes nothing.
  assert_eq!(run(lock.release_read::<()>()), Ok(false));
  assert_eq!(run(lock.release_write::<()>()), Ok(false));
}

#[test]
fn treentrant_lock_is_reentrant_within_one_fiber() {
  let lock = make(TReentrantLock::make());
  let l = lock.clone();
  // One transaction (one fiber): read, read, upgrade to write (sole reader), read as writer.
  let stm = {
    let (a, b, c, d) = (l.clone(), l.clone(), l.clone(), l.clone());
    a.acquire_read::<()>()
      .flat_map(move |_| b.acquire_read())
      .flat_map(move |reads| c.acquire_write().map(move |writes| (reads, writes)))
      .flat_map(move |(reads, writes)| d.acquire_read().map(move |r| (reads, writes, r)))
  };
  assert_eq!(run(stm), Ok((2, 1, 3)));
  assert_eq!(run(lock.is_locked::<()>()), Ok(true));
}

// ── contention ──────────────────────────────────────────────────────────────

const PER_FIBER: u32 = 500;
const FIBERS: u32 = 4;

type Step = Arc<dyn Fn(u32) -> Effect<(), ()> + Send + Sync>;

/// Runs `step(0..n)` in sequence.
fn times(i: u32, n: u32, step: Step) -> Effect<(), ()> {
  suspend(move || {
    if i == n {
      succeed(())
    } else {
      let next = Arc::clone(&step);
      step(i).flat_map(move |()| times(i + 1, n, next))
    }
  })
}

/// Runs `f` on a helper thread and fails the test if it does not finish in time (a lost
/// wake-up parks a fiber forever).
fn with_deadline<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
  let (tx, rx) = mpsc::channel();
  std::thread::spawn(move || {
    let _ = tx.send(f());
  });
  rx.recv_timeout(Duration::from_secs(120))
    .expect("contention run did not finish: a parked transaction was never woken")
}

#[test]
fn stm_collections_contention() {
  let queue = make(TPriorityQueue::<u32>::empty());
  let seen = make(TSet::<u32>::make());
  let counts = make(TArray::make(vec![0u32; FIBERS as usize]));
  let lock = make(TReentrantLock::make());
  let done = make(TPromise::<u32, ()>::make());
  let taken = make(TRef::make(0u32));

  // Producer fiber `f` offers its values; each offer also counts in the fiber's TArray slot,
  // under a read lock taken and released by the same fiber.
  let producer = |f: u32| -> Effect<(), ()> {
    let (queue, counts, lock) = (queue.clone(), counts.clone(), lock.clone());
    times(
      0,
      PER_FIBER,
      Arc::new(move |i| {
        let value = f * PER_FIBER + i;
        let (l1, l2) = (lock.clone(), lock.clone());
        let offer = {
          let counts = counts.clone();
          queue
            .offer::<()>(value)
            .flat_map(move |()| counts.update(f as usize, |n| n + 1).map(|_| ()))
        };
        commit(l1.acquire_read::<()>())
          .flat_map(move |_| commit(offer))
          .flat_map(move |()| commit(l2.release_read::<()>()).map(|_| ()))
      }),
    )
  };
  // Consumer fibers take (parking while empty), record each value in the set and the total,
  // and the one that takes the last value completes the promise.
  let consumer = || -> Effect<(), ()> {
    let (queue, seen, taken, done) = (queue.clone(), seen.clone(), taken.clone(), done.clone());
    times(
      0,
      PER_FIBER,
      Arc::new(move |_| {
        let (seen, taken, done) = (seen.clone(), taken.clone(), done.clone());
        commit(queue.take::<()>().flat_map(move |v| {
          let (taken, done) = (taken.clone(), done.clone());
          // No assertion on `add`'s result here: an attempt that will fail validation may see
          // a newer set. Uniqueness is checked on the committed set below.
          seen.add(v).flat_map(move |_| {
            let done = done.clone();
            taken.modify_stm(|n| (n + 1, n + 1)).flat_map(move |n| {
              if n == FIBERS * PER_FIBER {
                done.succeed(n).map(|_| ())
              } else {
                Stm::succeed(())
              }
            })
          })
        }))
      }),
    )
  };

  let fibers: Vec<Effect<(), ()>> = (0..FIBERS)
    .map(producer)
    .chain((0..FIBERS).map(|_| consumer()))
    .collect();
  let (total, exits) = with_deadline({
    let done = done.clone();
    move || {
      let rt = Runtime::new().expect("tokio runtime");
      let handles: Vec<_> = fibers.into_iter().map(|eff| rt.fork(eff)).collect();
      let total = rt.block_on(commit::<u32, (), HNil>(done.await_()));
      let exits: Vec<_> = handles
        .into_iter()
        .map(|h| rt.block_on(h.join::<HNil>()))
        .collect();
      (total, exits)
    }
  });

  assert_eq!(total, Exit::succeed(FIBERS * PER_FIBER));
  assert!(exits.iter().all(|e| *e == Exit::succeed(())));
  assert_eq!(run(seen.size::<()>()), Ok((FIBERS * PER_FIBER) as usize));
  assert_eq!(
    run(counts.to_vec::<()>()),
    Ok(vec![PER_FIBER; FIBERS as usize]),
    "no lost element update"
  );
  assert_eq!(run(queue.is_empty::<()>()), Ok(true));
  assert_eq!(
    run(lock.is_locked::<()>()),
    Ok(false),
    "every read lock released"
  );
}
