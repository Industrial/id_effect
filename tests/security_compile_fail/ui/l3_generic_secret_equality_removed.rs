// ADR 0013 R2 (L3): secrets compare only as `SecretString` / `SecretBytes`, in constant time;
// there is no generic (short-circuiting) `PartialEq` for `Secret<T>`.
use id_effect_kit::config::Secret;

fn main() {
  let pin = Secret::new(1234u32);
  let guess = Secret::new(1234u32);
  let _ = pin == guess;
}
