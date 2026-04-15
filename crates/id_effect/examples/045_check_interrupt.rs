//! Ex 045 — `check_interrupt` reads cancellation as an effect.
use id_effect::{CancellationToken, check_interrupt, run_blocking};

fn main() {
  let t = CancellationToken::new();
  assert_eq!(run_blocking(check_interrupt(&t), ()), Ok(false));
  t.cancel();
  assert_eq!(run_blocking(check_interrupt(&t), ()), Ok(true));
  println!("045_check_interrupt ok");
}
