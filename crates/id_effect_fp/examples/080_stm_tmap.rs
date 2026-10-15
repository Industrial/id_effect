//! Ex 080 — `TMap` transactional key/value operations.
use id_effect::Effect;
use id_effect::{Cause, Exit};
use id_effect_fp::{TMap, commit};

fn main() {
  let stm = TMap::<&'static str, i32>::make().flat_map(|m| {
    m.set("k", 10)
      .flat_map(move |_| m.get(&"k").map(|opt| opt.expect("some")))
  });
  assert_eq!(run(commit(stm)), Ok(10));
  println!("080_stm_tmap ok");
}

/// Runs `eff` on the global runtime: `Ok` on success, `Err` on a typed failure.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
