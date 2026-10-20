// ADR 0013 R2 (M13): a `Secret` is zeroized on drop, so it only holds values that can be
// zeroized; a borrowed `&str` (whose bytes it does not own) is refused.
use id_effect_kit::config::Secret;

fn main() {
  let _ = Secret::new("hunter2");
}
