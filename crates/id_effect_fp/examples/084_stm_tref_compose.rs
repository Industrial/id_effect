//! Ex 084 — `TRef` operations composed into one transaction (book: "TRef").
//!
//! Run: `cargo run -p id_effect_fp --example 084_stm_tref_compose`
use std::thread;

use id_effect::run_blocking;
use id_effect_fp::{Stm, TRef, commit};

/// Increment `counter` and add its old value to `total`, atomically.
fn bump(counter: &TRef<i32>, total: &TRef<i32>) -> Stm<(), ()> {
  let (counter, total) = (counter.clone(), total.clone());
  counter.read_stm().flat_map(move |count| {
    let (counter, total) = (counter.clone(), total.clone());
    total.read_stm().flat_map(move |sum| {
      let total = total.clone();
      counter
        .write_stm(count + 1)
        .flat_map(move |()| total.write_stm(sum + count))
    })
  })
}

fn main() {
  // Cells are created once (a `TRef::make` transaction) and then shared by cloning.
  let counter = run_blocking(commit(TRef::make(0_i32)), ()).expect("cell");
  let total = run_blocking(commit(TRef::make(0_i32)), ()).expect("cell");

  for _ in 0..3 {
    run_blocking(commit::<_, _, ()>(bump(&counter, &total)), ()).expect("bump");
  }
  let read = |r: &TRef<i32>| run_blocking(commit(r.read_stm::<()>()), ()).expect("read");
  assert_eq!((read(&counter), read(&total)), (3, 3)); // 0 + 1 + 2

  // `modify_stm` returns a value computed from the old state: here, the previous count.
  let previous = run_blocking(commit(counter.modify_stm::<_, (), _>(|n| (n, n * 10))), ());
  assert_eq!(previous, Ok(3));

  // `TRef` is `Clone + Send + Sync`: clone it into each thread, no `Arc<Mutex<_>>` needed.
  let hits = run_blocking(commit(TRef::make(0_u32)), ()).expect("cell");
  let handles: Vec<_> = (0..2)
    .map(|_| {
      let hits = hits.clone();
      thread::spawn(move || {
        for _ in 0..1_000 {
          run_blocking(commit(hits.update_stm::<(), _>(|n| n + 1)), ()).expect("update");
        }
      })
    })
    .collect();
  for h in handles {
    h.join().expect("join");
  }
  assert_eq!(run_blocking(commit(hits.read_stm::<()>()), ()), Ok(2_000));
  println!("084_stm_tref_compose ok");
}
