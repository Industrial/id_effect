//! Ex 017 — `Cause::fail_option` extracts `Fail` payloads only.
use id_effect::Cause;

fn main() {
  assert_eq!(Cause::fail(7_u8).fail_option(), Some(7));
  assert_eq!(Cause::<u8>::die("x").fail_option(), None);
  println!("017_cause_optional ok");
}
