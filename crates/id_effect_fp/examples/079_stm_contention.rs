//! Ex 079 — Two transactions can serialize on the same `TRef` via STM restart.
use id_effect::Effect;
use id_effect::{Cause, Exit};
use id_effect_fp::{TRef, commit};

fn main() {
  let shared = run(commit(TRef::make(0_i32))).expect("cell");
  let s1 = shared.clone();
  let h = std::thread::spawn(move || {
    let _ = run(commit(s1.update_stm::<(), _>(|x| x + 1)));
  });
  h.join().expect("join");
  let _ = run(commit(shared.update_stm::<(), _>(|x| x + 41)));
  let v = run(commit(shared.read_stm::<()>()));
  assert_eq!(v, Ok::<i32, ()>(42));
  println!("079_stm_contention ok");
}

/// Runs `eff` on the global runtime: `Ok` on success, `Err` on a typed failure.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
