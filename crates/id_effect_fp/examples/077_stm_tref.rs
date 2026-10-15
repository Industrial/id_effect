//! Ex 077 — `TRef` read/write inside `commit`.
use id_effect::next::Effect;
use id_effect::{Cause, Exit};
use id_effect_fp::{TRef, commit};

fn main() {
  let stm = TRef::make(0_i32).flat_map(|cell| {
    let c1 = cell.clone();
    let c2 = cell.clone();
    cell
      .read_stm::<()>()
      .flat_map(move |v| {
        assert_eq!(v, 0);
        c1.write_stm::<()>(42)
      })
      .flat_map(move |_| c2.read_stm::<()>())
  });
  assert_eq!(run(commit(stm)), Ok(42));
  println!("077_stm_tref ok");
}

/// Runs `eff` on the global next runtime: `Ok` on success, `Err` on a typed failure.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
