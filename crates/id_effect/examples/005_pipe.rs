//! Ex 005 — `.pipe` and `pipe!` for left-to-right composition.
use id_effect::{Effect, Pipe, pipe, pure, run_blocking, succeed};

fn add_five(eff: Effect<i32, (), ()>) -> Effect<i32, (), ()> {
  eff.map(|n| n + 5)
}
fn times_three(eff: Effect<i32, (), ()>) -> Effect<i32, (), ()> {
  eff.flat_map(|n| succeed(n * 3))
}
fn minus_two(eff: Effect<i32, (), ()>) -> Effect<i32, (), ()> {
  eff.map(|n| n - 2)
}

fn main() {
  let dot = pure(10).pipe(add_five).pipe(times_three).pipe(minus_two);
  let mac = pipe!(pure(10), add_five, times_three, minus_two);
  assert_eq!(run_blocking(dot, ()), Ok(43));
  assert_eq!(run_blocking(mac, ()), Ok(43));
  println!("005_pipe ok");
}
