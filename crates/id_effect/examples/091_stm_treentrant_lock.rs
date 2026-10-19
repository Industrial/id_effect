//! Ex 091 — `TReentrantLock`: read/write lock owned by fibers, reentrant per fiber (book:
//! "Transactional Collections").
//!
//! Run: `cargo run -p id_effect --example 091_stm_treentrant_lock`
use std::thread;

use id_effect::Effect;
use id_effect::stm::{Stm, TReentrantLock, TRef, commit};
use id_effect::{Cause, Exit};

fn main() {
  let lock = run(commit(TReentrantLock::make())).expect("lock");
  let gate = run(commit(TRef::make(false))).expect("cell");

  // One fiber takes the write lock twice (reentrant) and holds it until the gate opens. The
  // lock owner is the fiber running `commit`, so acquire and release run in one effect chain.
  let writer = {
    let (l1, l2, l3, l4) = (lock.clone(), lock.clone(), lock.clone(), lock.clone());
    let gate = gate.clone();
    thread::spawn(move || {
      let eff = commit(l1.acquire_write::<()>())
        .flat_map(move |_| commit(l2.acquire_write::<()>()))
        .flat_map(move |held| {
          commit(gate.read_stm::<()>().flat_map(Stm::<(), ()>::check)).map(move |()| held)
        })
        .flat_map(move |held| commit(l3.release_write::<()>()).map(move |_| held))
        .flat_map(move |held| commit(l4.release_write::<()>()).map(move |_| held));
      run(eff).expect("writer")
    })
  };
  // Wait until the writer holds the lock.
  run(commit(
    lock.is_write_locked::<()>().flat_map(Stm::<(), ()>::check),
  ))
  .expect("locked");

  // Readers on other fibers are excluded: a try-lock (`or_else`) fails, a reader waits.
  let try_read = lock
    .acquire_read::<()>()
    .map(|_| true)
    .or_else(Stm::succeed(false));
  assert_eq!(run(commit(try_read)), Ok(false));
  let reader = {
    let (l1, l2) = (lock.clone(), lock.clone());
    thread::spawn(move || {
      let eff = commit(l1.acquire_read::<()>())
        .flat_map(move |n| commit(l2.release_read::<()>()).map(move |_| n));
      run(eff).expect("reader")
    })
  };

  run(commit(gate.write_stm::<()>(true))).expect("open gate");
  assert_eq!(
    writer.join().expect("join"),
    2,
    "the writer held two write locks"
  );
  assert_eq!(reader.join().expect("join"), 1);
  assert_eq!(run(commit(lock.is_locked::<()>())), Ok(false));
  println!("091_stm_treentrant_lock ok");
}

/// Runs `eff` on the global runtime: `Ok` on success, `Err` on a typed failure.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
