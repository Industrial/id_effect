//! Ex 024 — `get_mut` mutates a tagged cell in place.
use id_effect::{ctx, service_key};

service_key!(struct ScoreKey);

fn main() {
  let mut env = ctx!(ScoreKey => 10_i32);
  *env.get_mut::<ScoreKey>() = 42;
  assert_eq!(*env.get::<ScoreKey>(), 42);
  println!("024_get_mut ok");
}
