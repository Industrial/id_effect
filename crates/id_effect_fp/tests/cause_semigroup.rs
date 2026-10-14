//! `Semigroup` instance for `id_effect::Cause` (moved from `id_effect::failure::cause`).

use id_effect::Cause;
use id_effect_fp::Semigroup;

#[test]
fn combine_two_causes_returns_both_variant() {
  let a = Cause::fail("a");
  let b = Cause::fail("b");
  assert_eq!(
    a.clone().combine(b.clone()),
    Cause::both(Cause::fail("a"), Cause::fail("b"))
  );
}

#[test]
fn combine_is_associative_law() {
  let a = Cause::fail(1u8);
  let b = Cause::fail(2u8);
  let c = Cause::fail(3u8);
  let lhs = a.clone().combine(b.clone()).combine(c.clone());
  let rhs = a.clone().combine(b.clone().combine(c.clone()));
  assert_eq!(
    lhs,
    Cause::both(
      Cause::both(Cause::fail(1u8), Cause::fail(2u8)),
      Cause::fail(3u8)
    )
  );
  assert_eq!(
    rhs,
    Cause::both(
      Cause::fail(1u8),
      Cause::both(Cause::fail(2u8), Cause::fail(3u8))
    )
  );
}
