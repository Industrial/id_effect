//! Ex 077 — `TRef` read/write inside `commit`.
use id_effect::{TRef, commit, run_blocking};

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
  assert_eq!(run_blocking(commit(stm), ()), Ok(42));
  println!("077_stm_tref ok");
}
