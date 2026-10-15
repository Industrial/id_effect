//! Ex 081 — `TQueue::offer` / `take` with retry on empty.
use id_effect::Effect;
use id_effect::{Cause, Exit};
use id_effect_fp::{TQueue, commit};

fn main() {
  let stm = TQueue::bounded(4).flat_map(|q| {
    let q1 = q.clone();
    let q2 = q.clone();
    q.offer(1_i32)
      .flat_map(move |_| q1.offer(2))
      .flat_map(move |_| q2.take::<()>())
  });
  assert_eq!(run(commit(stm)), Ok(1));
  println!("081_stm_tqueue ok");
}

/// Runs `eff` on the global next runtime: `Ok` on success, `Err` on a typed failure.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
