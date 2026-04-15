//! Ex 097 — `run_test` returns `Exit` and runs harness hygiene checks.
use id_effect::{Exit, fail, run_test, succeed};

fn main() {
  assert_eq!(
    run_test(succeed::<u16, (), ()>(42_u16), ()),
    Exit::succeed(42)
  );
  assert_eq!(
    run_test(fail::<(), &'static str, ()>("no"), ()),
    Exit::fail("no")
  );
  println!("097_run_test ok");
}
