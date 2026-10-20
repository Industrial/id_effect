//! Ex 089 — `TPriorityQueue`: `take` returns the smallest element and waits while the queue is
//! empty (book: "Transactional Collections").
//!
//! Run: `cargo run -p id_effect --example 089_stm_tpriority_queue`
use std::thread;

use id_effect::Effect;
use id_effect::stm::{TPriorityQueue, commit};
use id_effect::{Cause, Exit};

/// A job ordered by priority first (lower runs first), then by name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Job {
  priority: u8,
  name: &'static str,
}

fn main() {
  let jobs = run(commit(TPriorityQueue::<Job>::empty())).expect("queue");

  // The worker starts first and waits (STM retry, the fiber parks) until jobs arrive.
  let worker = {
    let jobs = jobs.clone();
    thread::spawn(move || run(commit(jobs.take::<()>())).expect("take"))
  };
  run(commit(jobs.offer_all::<()>(vec![
    Job {
      priority: 2,
      name: "send report",
    },
    Job {
      priority: 0,
      name: "page on-call",
    },
    Job {
      priority: 1,
      name: "rotate logs",
    },
  ])))
  .expect("offer");
  let first = worker.join().expect("join");
  // All three jobs land in one commit, so the woken worker sees the most urgent one.
  assert_eq!(first.name, "page on-call");

  assert_eq!(
    run(commit(jobs.peek::<()>())).map(|j| j.name),
    Ok("rotate logs")
  );
  let rest: Vec<_> = run(commit(jobs.take_all::<()>()))
    .expect("take_all")
    .into_iter()
    .map(|j| j.name)
    .collect();
  assert_eq!(rest, ["rotate logs", "send report"]);

  // `take_option` never waits; `or_else` picks another queue when this one is empty.
  assert_eq!(run(commit(jobs.take_option::<()>())), Ok(None));
  let fallback = run(commit(TPriorityQueue::from_values([Job {
    priority: 9,
    name: "idle",
  }])))
  .expect("queue");
  let next = jobs.take::<()>().or_else(fallback.take()).map(|j| j.name);
  assert_eq!(run(commit(next)), Ok("idle"));
  println!("089_stm_tpriority_queue ok");
}

/// Runs `eff` on the global runtime: `Ok` on success, `Err` on a typed failure.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
