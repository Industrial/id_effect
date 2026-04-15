//! Ex 094 — `with_span` pushes a logical span around nested work.
use id_effect::{Never, run_blocking, succeed, with_span};

fn main() {
  let eff = with_span(succeed::<u8, Never, ()>(9), "demo.span");
  assert_eq!(run_blocking(eff, ()), Ok(9));
  println!("094_tracing_spans ok");
}
