//! Ex 086 — `TQueue`, `TMap`, `TSemaphore` (book: "Transactional Collections").
//!
//! Run: `cargo run -p id_effect_fp --example 086_stm_collections`
use std::thread;

use id_effect::run_blocking;
use id_effect_fp::{Stm, TMap, TQueue, TRef, TSemaphore, commit};

/// `TQueue::offer` returns `false` when a bounded queue is full; `check` turns that into a retry,
/// so this offer waits for space.
fn offer_blocking(queue: &TQueue<u32>, job: u32) -> Stm<(), ()> {
  queue.offer(job).flat_map(Stm::<(), ()>::check)
}

fn main() {
  // Producer/consumer over a queue of capacity 2: `take` retries while empty, the producer
  // retries while full.
  let queue = run_blocking(commit(TQueue::<u32>::bounded(2)), ()).expect("queue");
  let producer = {
    let queue = queue.clone();
    thread::spawn(move || {
      for job in 1..=10 {
        run_blocking(commit(offer_blocking(&queue, job)), ()).expect("offer");
      }
    })
  };
  let sum: u32 = (0..10)
    .map(|_| run_blocking(commit(queue.take::<()>()), ()).expect("take"))
    .sum();
  producer.join().expect("join");
  assert_eq!(sum, 55);

  // Reading a TMap and bumping a TRef in one transaction: both happen, or neither.
  let users = run_blocking(commit(TMap::<String, String>::make()), ()).expect("map");
  let reads = run_blocking(commit(TRef::make(0_u32)), ()).expect("cell");
  run_blocking(commit(users.set::<()>("alice".into(), "Alice".into())), ()).expect("set");
  let lookup = {
    let reads = reads.clone();
    users
      .get::<()>(&"alice".to_string())
      .flat_map(move |user| reads.update_stm(|n| n + 1).map(move |()| user.clone()))
  };
  assert_eq!(
    run_blocking(commit(lookup), ()),
    Ok(Some("Alice".to_string()))
  );
  assert_eq!(run_blocking(commit(reads.read_stm::<()>()), ()), Ok(1));

  // TSemaphore: `acquire` retries while no permits are left.
  let sem = run_blocking(commit(TSemaphore::make(1)), ()).expect("sem");
  run_blocking(commit(sem.acquire::<()>()), ()).expect("acquire");
  let try_acquire = sem
    .acquire::<()>()
    .map(|()| true)
    .or_else(Stm::succeed(false));
  assert_eq!(
    run_blocking(commit(try_acquire.clone()), ()),
    Ok(false),
    "no permit left"
  );
  run_blocking(commit(sem.release::<()>()), ()).expect("release");
  assert_eq!(run_blocking(commit(try_acquire), ()), Ok(true));
  println!("086_stm_collections ok");
}
