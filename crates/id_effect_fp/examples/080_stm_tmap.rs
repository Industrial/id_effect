//! Ex 080 — `TMap` transactional key/value operations.
use id_effect::run_blocking;
use id_effect_fp::{TMap, commit};

fn main() {
  let stm = TMap::<&'static str, i32>::make().flat_map(|m| {
    m.set("k", 10)
      .flat_map(move |_| m.get(&"k").map(|opt| opt.expect("some")))
  });
  assert_eq!(run_blocking(commit(stm), ()), Ok(10));
  println!("080_stm_tmap ok");
}
