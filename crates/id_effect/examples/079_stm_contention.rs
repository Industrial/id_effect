//! Ex 079 — Two transactions can serialize on the same `TRef` via STM restart.
use id_effect::{TRef, commit, run_blocking};

fn main() {
  let shared = run_blocking(commit(TRef::make(0_i32)), ()).expect("cell");
  let s1 = shared.clone();
  let h = std::thread::spawn(move || {
    let _ = run_blocking(commit(s1.update_stm::<(), _>(|x| x + 1)), ());
  });
  h.join().expect("join");
  let _ = run_blocking(commit(shared.update_stm::<(), _>(|x| x + 41)), ());
  let v = run_blocking(commit(shared.read_stm::<()>()), ());
  assert_eq!(v, Ok::<i32, ()>(42));
  println!("079_stm_contention ok");
}
