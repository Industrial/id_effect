//! Ex 082 — `TSemaphore` acquire/release in a transaction.
use id_effect::Effect;
use id_effect::{Cause, Exit};
use id_effect_fp::{TSemaphore, commit};

fn main() {
  let stm =
    TSemaphore::make(1).flat_map(|s| s.acquire::<()>().flat_map(move |_| s.release::<()>()));
  run(commit(stm)).expect("commit");
  println!("082_stm_tsemaphore ok");
}

/// Runs `eff` on the global runtime: `Ok` on success, `Err` on a typed failure.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
