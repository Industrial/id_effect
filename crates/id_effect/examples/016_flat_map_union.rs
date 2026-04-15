//! Ex 016 — `flat_map_union` sequences heterogeneous error channels into `Or`.
use id_effect::{Or, fail, run_blocking, succeed};

#[derive(Debug, Clone, PartialEq, Eq)]
enum E1 {
  Bad,
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum E2 {
  Worse,
}

fn step1() -> id_effect::Effect<i32, E1, ()> {
  succeed::<i32, E1, ()>(21)
}
fn step2(n: i32) -> id_effect::Effect<i32, E2, ()> {
  if n == 21 {
    succeed::<i32, E2, ()>(n * 2)
  } else {
    fail(E2::Worse)
  }
}

fn main() {
  let ok = step1().flat_map_union(step2);
  assert_eq!(run_blocking(ok, ()), Ok(42));

  let bad = fail::<i32, E1, ()>(E1::Bad).flat_map_union(step2);
  assert_eq!(run_blocking(bad, ()), Err(Or::Left(E1::Bad)));
  println!("016_flat_map_union ok");
}
