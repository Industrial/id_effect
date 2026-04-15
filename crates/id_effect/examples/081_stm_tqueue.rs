//! Ex 081 — `TQueue::offer` / `take` with retry on empty.
use id_effect::{TQueue, commit, run_blocking};

fn main() {
  let stm = TQueue::bounded(4).flat_map(|q| {
    let q1 = q.clone();
    let q2 = q.clone();
    q.offer(1_i32)
      .flat_map(move |_| q1.offer(2))
      .flat_map(move |_| q2.take::<()>())
  });
  assert_eq!(run_blocking(commit(stm), ()), Ok(1));
  println!("081_stm_tqueue ok");
}
