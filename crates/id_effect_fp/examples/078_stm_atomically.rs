//! Ex 078 — `atomically` is [`commit`] under the Effect.ts-style name.
use id_effect::run_blocking;
use id_effect_fp::{TRef, atomically, commit};

fn main() {
  let stm = TRef::make(0_i32).flat_map(|cell| {
    let c2 = cell.clone();
    cell
      .write_stm::<()>(7)
      .flat_map(move |_| c2.read_stm::<()>())
  });
  let via_atomic = run_blocking(atomically(stm), ()).expect("atomically");
  let stm2 = TRef::make(0_i32).flat_map(|cell| {
    let c2 = cell.clone();
    cell
      .write_stm::<()>(7)
      .flat_map(move |_| c2.read_stm::<()>())
  });
  let via_commit = run_blocking(commit(stm2), ()).expect("commit");
  assert_eq!(via_atomic, via_commit);
  assert_eq!(via_atomic, 7);
  println!("078_stm_atomically ok");
}
