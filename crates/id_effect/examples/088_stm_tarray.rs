//! Ex 088 — `TArray`: one cell per element, so updates to different slots do not conflict
//! (book: "Transactional Collections").
//!
//! Run: `cargo run -p id_effect --example 088_stm_tarray`
use std::thread;

use id_effect::Effect;
use id_effect::stm::{Stm, TArray, commit};
use id_effect::{Cause, Exit};

fn main() {
  // Four account balances; a transfer moves money between two slots in one transaction.
  let balances = run(commit(TArray::make(vec![100_i64, 100, 100, 100]))).expect("array");
  let transfer = |from: usize, to: usize, amount: i64| {
    let b = balances.clone();
    balances.get::<&str>(from).flat_map(move |bal| match bal {
      Some(bal) if bal >= amount => {
        let b2 = b.clone();
        b.update(from, move |n| n - amount)
          .flat_map(move |_| b2.update(to, move |n| n + amount))
          .map(|_| ())
      }
      Some(_) => Stm::fail("insufficient funds"),
      None => Stm::fail("no such account"),
    })
  };

  // Concurrent transfers in a ring: the total never changes.
  let workers: Vec<_> = (0..4)
    .map(|i| {
      let t = transfer(i, (i + 1) % 4, 1);
      thread::spawn(move || {
        for _ in 0..50 {
          run(commit(t.clone())).expect("transfer");
        }
      })
    })
    .collect();
  for w in workers {
    w.join().expect("join");
  }
  assert_eq!(run(commit(balances.to_vec::<()>())), Ok(vec![100; 4]));
  assert_eq!(
    run(commit(balances.fold::<i64, (), _>(0, |a, b| a + b))),
    Ok(400)
  );

  // A failed transfer changes nothing.
  assert_eq!(
    run(commit(transfer(0, 1, 1_000))),
    Err("insufficient funds")
  );
  assert_eq!(run(commit(transfer(9, 1, 1))), Err("no such account"));
  assert_eq!(run(commit(balances.get::<()>(0))), Ok(Some(100)));
  println!("088_stm_tarray ok");
}

/// Runs `eff` on the global runtime: `Ok` on success, `Err` on a typed failure.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
