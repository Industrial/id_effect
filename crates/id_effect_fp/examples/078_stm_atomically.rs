//! Ex 078 — `atomically` is [`commit`] under the Effect.ts-style name.
use id_effect::Effect;
use id_effect::{Cause, Exit};
use id_effect_fp::{TRef, atomically, commit};

fn main() {
  let stm = TRef::make(0_i32).flat_map(|cell| {
    let c2 = cell.clone();
    cell
      .write_stm::<()>(7)
      .flat_map(move |_| c2.read_stm::<()>())
  });
  let via_atomic = run(atomically(stm)).expect("atomically");
  let stm2 = TRef::make(0_i32).flat_map(|cell| {
    let c2 = cell.clone();
    cell
      .write_stm::<()>(7)
      .flat_map(move |_| c2.read_stm::<()>())
  });
  let via_commit = run(commit(stm2)).expect("commit");
  assert_eq!(via_atomic, via_commit);
  assert_eq!(via_atomic, 7);
  println!("078_stm_atomically ok");
}

/// Runs `eff` on the global next runtime: `Ok` on success, `Err` on a typed failure.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
