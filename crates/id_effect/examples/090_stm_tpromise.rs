//! Ex 090 — `TPromise`: a write-once cell; `await_` waits until it is completed (book:
//! "Transactional Collections").
//!
//! Run: `cargo run -p id_effect --example 090_stm_tpromise`
use std::thread;

use id_effect::Effect;
use id_effect::stm::{Stm, TPromise, TRef, commit};
use id_effect::{Cause, Exit};

fn main() {
  let config = run(commit(TPromise::<String, String>::make())).expect("promise");
  let readers = run(commit(TRef::make(0_u32))).expect("cell");

  // Readers wait (the fiber parks) until the config is loaded, then count themselves in the
  // same transaction.
  let waiters: Vec<_> = (0..3)
    .map(|_| {
      let (config, readers) = (config.clone(), readers.clone());
      thread::spawn(move || {
        let read = config
          .await_()
          .flat_map(move |c| readers.update_stm(|n| n + 1).map(move |()| c.clone()));
        run(commit(read)).expect("await")
      })
    })
    .collect();

  // Completes once: the first completion wins, later ones return `false`.
  assert_eq!(
    run(commit(config.succeed::<()>("db=primary".into()))),
    Ok(true)
  );
  assert_eq!(
    run(commit(config.succeed::<()>("db=replica".into()))),
    Ok(false)
  );
  for w in waiters {
    assert_eq!(w.join().expect("join"), "db=primary");
  }
  assert_eq!(run(commit(readers.read_stm::<()>())), Ok(3));

  // A failed promise fails every awaiting transaction with its error.
  let failed = run(commit(TPromise::<String, String>::make())).expect("promise");
  run(commit(failed.fail::<()>("config server down".into()))).expect("fail");
  assert_eq!(
    run(commit(failed.await_())),
    Err("config server down".to_string())
  );

  // `or_else` gives a default instead of waiting on a pending promise.
  let pending = run(commit(TPromise::<String, String>::make())).expect("promise");
  let with_default = pending
    .await_()
    .or_else(Stm::succeed("defaults".to_string()));
  assert_eq!(run(commit(with_default)), Ok("defaults".to_string()));
  assert_eq!(run(commit(pending.is_done::<()>())), Ok(false));
  println!("090_stm_tpromise ok");
}

/// Runs `eff` on the global runtime: `Ok` on success, `Err` on a typed failure.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
