//! Ex 044 — Child tokens inherit parent cancellation.
use id_effect::CancellationToken;

fn main() {
  let p = CancellationToken::new();
  let c = p.child_token();
  p.cancel();
  assert!(c.is_cancelled());
  println!("044_cancellation_tree ok");
}
