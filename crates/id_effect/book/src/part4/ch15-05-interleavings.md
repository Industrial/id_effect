# Exploring Interleavings with TestExecutor

`TestExecutor::new()` runs ready fibers in FIFO order. That makes a test deterministic, but it
also means the test sees **one** interleaving of its fibers. A race that only shows up when two
fibers' steps interleave differently passes every time.

A **seeded** executor explores the others. `TestExecutor::seeded(seed)` is the same executor
(virtual clock, `run_until_stalled`, `advance`) except that each poll picks one of the queued
tasks with a SplitMix64 generator seeded by `seed`, instead of the front one. Every fiber is one
executor task, so the seed selects the order in which ready fibers run, and how their steps
between suspension points (`yield_fiber`, a sleep, a wait on a queue or a `Deferred`)
interleave.

| API | Meaning |
|-----|---------|
| `TestExecutor::seeded(seed)` | an executor whose scheduling choices come from `seed` |
| `exec.seed()` | `Some(seed)` for a seeded executor, `None` for `TestExecutor::new()` |
| `explore_interleavings(seeds, test)` | runs `test(&exec)` on `TestExecutor::seeded(s)` for each `s` in `seeds`; `Err(InterleavingFailure { seed, message })` for the first that panics |
| `assert_interleavings(seeds, test)` | the same, panicking with the failing seed |

The same seed driven the same way from the test thread makes the same choices, so a failure
reproduces by running the test body again on `TestExecutor::seeded(failure.seed)`.

## A planted race

Two fibers increment a shared counter by reading it, yielding, and writing back what they read
plus one: a lost update waiting to happen. The second fiber also yields before it reads.

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use id_effect::{Effect, TestExecutor, explore_interleavings, succeed, sync, yield_fiber, zip_par};

fn bump(counter: &Arc<AtomicU64>, pause_first: bool) -> Effect<()> {
  let (read, write) = (Arc::clone(counter), Arc::clone(counter));
  let pause: Effect<()> = if pause_first { yield_fiber() } else { succeed(()) };
  pause
    .flat_map(move |()| sync(move || read.load(Ordering::SeqCst)))
    .flat_map(move |v| yield_fiber().map(move |()| v)) // the race window
    .flat_map(move |v| sync(move || write.store(v + 1, Ordering::SeqCst)))
}

/// The test body: drive both fibers to completion, then check the count.
fn two_bumps(exec: &TestExecutor) {
  let counter = Arc::new(AtomicU64::new(0));
  let h = zip_par(bump(&counter, false), bump(&counter, true)).fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  assert!(h.is_done());
  assert_eq!(counter.load(Ordering::SeqCst), 2, "lost update");
}

// FIFO order: the first fiber reads and yields, the second yields, the first writes 1, the
// second reads 1 and writes 2. The race is never seen.
two_bumps(&TestExecutor::new());

// Seeded orders: some seed lets both fibers read 0 before either writes.
let failure = explore_interleavings(0..64, two_bumps).expect_err("the race is found");
assert!(failure.message.contains("lost update"));
println!("{failure}"); // "interleaving test failed under seed N: ... TestExecutor::seeded(N)"

// The reported seed reproduces it.
assert_eq!(explore_interleavings([failure.seed], two_bumps), Err(failure.clone()));
```

Once the race is fixed (here: an atomic `fetch_add`, or a `Ref::update`), the same exploration
passes, and a regression test can keep it that way:

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use id_effect::{Effect, TestExecutor, assert_interleavings, sync, yield_fiber, zip_par};

fn bump(counter: &Arc<AtomicU64>) -> Effect<()> {
  let counter = Arc::clone(counter);
  yield_fiber().flat_map(move |()| {
    sync(move || {
      counter.fetch_add(1, Ordering::SeqCst);
    })
  })
}

assert_interleavings(0..256, |exec: &TestExecutor| {
  let counter = Arc::new(AtomicU64::new(0));
  let h = zip_par(bump(&counter), bump(&counter)).fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  assert!(h.is_done());
  assert_eq!(counter.load(Ordering::SeqCst), 2);
});
```

## Writing interleaving tests

- **Build everything inside the test body.** `explore_interleavings` gives each seed a fresh
  executor; shared state created outside the closure leaks from one seed's run into the next.
- **Drive the executor in the body** with `run_until_stalled` and, for timers, `advance`. Seeded
  order and virtual time compose: sleeps still fire in deadline order, and the tasks they wake
  are then picked by the seed.
- **Choose the seed range by cost.** Each seed is one full run; a few hundred seeds of a small
  test take milliseconds. `explore_interleavings` stops at the first failing seed.
- **Pin a found seed.** Keep a regression test on `TestExecutor::seeded(seed)` (or
  `explore_interleavings([seed], ..)`) next to the wider sweep.
- **Determinism needs one driving thread.** A wake from another OS thread queues its task at
  whatever moment it happens, so a test that involves real threads is reproducible only as far
  as those threads are.

The seed chooses among the tasks that are *queued* at each step; it does not split a fiber's
work between two suspension points. A race whose window contains no suspension point (no
`yield_fiber`, sleep or wait) cannot interleave on any executor, seeded or not.
