//! [`Cause`] — the failure ADT for typed errors, defects, interrupts, and composite causes.
//!
//! [`Cause`] is compared with derived [`PartialEq`] / [`Eq`]. For Effect.ts-style
//! **structural data** keys (`HashMap`, `HashSet`, schema-like APIs), implement
//! [`Hash`](std::hash::Hash) for `E` and use [`crate::schema::data::EffectData`] as the capability
//! marker (via the blanket impl once `PartialEq + Eq + Hash` hold).

use core::fmt;

use crate::Matcher;
use crate::algebra::semigroup::Semigroup;
use crate::foundation::option_::option;
use crate::runtime::FiberId;

/// Effect failure algebra: typed errors, defects, fiber interrupts, and composite causes.
#[derive(Clone, Debug, crate::EffectData)]
pub enum Cause<E> {
  /// Recoverable failure carrying the typed error `E`.
  Fail(E),
  /// Defect (panic-style) message.
  Die(String),
  /// Fiber interrupted at `fiber_id`.
  Interrupt(FiberId),
  /// Parallel composition of two causes.
  Both(Box<Cause<E>>, Box<Cause<E>>),
  /// Sequential composition: `left` observed before `right`.
  Then(Box<Cause<E>>, Box<Cause<E>>),
}

impl<E> Cause<E> {
  /// [`Cause::Fail`] wrapping `error`.
  #[inline]
  pub fn fail(error: E) -> Self {
    Self::Fail(error)
  }

  /// [`Cause::Die`] with `message`.
  #[inline]
  pub fn die(message: impl Into<String>) -> Self {
    Self::Die(message.into())
  }

  /// [`Cause::Interrupt`] for `fiber_id`.
  #[inline]
  pub fn interrupt(fiber_id: FiberId) -> Self {
    Self::Interrupt(fiber_id)
  }

  /// [`Cause::Both`] combining `left` and `right`.
  #[inline]
  pub fn both(left: Cause<E>, right: Cause<E>) -> Self {
    Self::Both(Box::new(left), Box::new(right))
  }

  /// [`Cause::Then`] sequencing `left` before `right`.
  #[inline]
  pub fn then(left: Cause<E>, right: Cause<E>) -> Self {
    Self::Then(Box::new(left), Box::new(right))
  }

  /// Map [`Cause::Fail`] payloads with `map`; preserve other variants.
  pub fn map_fail<E2, F>(self, map: F) -> Cause<E2>
  where
    F: Fn(E) -> E2 + Copy,
  {
    match self {
      Cause::Fail(error) => Cause::Fail(map(error)),
      Cause::Die(message) => Cause::Die(message),
      Cause::Interrupt(fiber_id) => Cause::Interrupt(fiber_id),
      Cause::Both(left, right) => {
        Cause::Both(Box::new(left.map_fail(map)), Box::new(right.map_fail(map)))
      }
      Cause::Then(left, right) => {
        Cause::Then(Box::new(left.map_fail(map)), Box::new(right.map_fail(map)))
      }
    }
  }

  /// [`Cause::Fail`] payload as `Some`, every other variant as `None`.
  ///
  /// Uses [`crate::foundation::option_::option::from_predicate`] on the failure value.
  #[inline]
  pub fn fail_option(self) -> Option<E> {
    match self {
      Cause::Fail(e) => option::from_predicate(e, |_| true),
      _ => option::none(),
    }
  }

  /// Return `true` if `pred` matches this cause or any sub-cause (depth-first).
  pub fn contains<F>(&self, pred: &F) -> bool
  where
    F: Fn(&Cause<E>) -> bool,
  {
    if pred(self) {
      return true;
    }
    match self {
      Cause::Both(l, r) | Cause::Then(l, r) => l.contains(pred) || r.contains(pred),
      _ => false,
    }
  }

  /// Human-readable tree of this cause (for debugging and logs).
  pub fn pretty(&self) -> String
  where
    E: fmt::Display + Clone + 'static,
  {
    let input = self.clone();
    Matcher::<Cause<E>, String>::new()
      .when(
        Box::new(|c: &Cause<E>| matches!(c, Cause::Fail(_))),
        |c| match c {
          Cause::Fail(error) => format!("Fail({error})"),
          _ => unreachable!(),
        },
      )
      .when(
        Box::new(|c: &Cause<E>| matches!(c, Cause::Die(_))),
        |c| match c {
          Cause::Die(message) => format!("Die({message})"),
          _ => unreachable!(),
        },
      )
      .when(
        Box::new(|c: &Cause<E>| matches!(c, Cause::Interrupt(_))),
        |c| match c {
          Cause::Interrupt(fiber_id) => format!("Interrupt({fiber_id})"),
          _ => unreachable!(),
        },
      )
      .when(
        Box::new(|c: &Cause<E>| matches!(c, Cause::Both(_, _))),
        |c| match c {
          Cause::Both(left, right) => format!("Both({}, {})", left.pretty(), right.pretty()),
          _ => unreachable!(),
        },
      )
      .when(
        Box::new(|c: &Cause<E>| matches!(c, Cause::Then(_, _))),
        |c| match c {
          Cause::Then(left, right) => format!("Then({}, {})", left.pretty(), right.pretty()),
          _ => unreachable!(),
        },
      )
      .run_exhaustive(input)
  }
}

impl<E> Cause<Cause<E>> {
  /// Flatten a `Cause<Cause<E>>` into `Cause<E>`.
  ///
  /// `Fail(inner)` unwraps to `inner`; structural variants (`Both`, `Then`) are
  /// recursively flattened; `Die` and `Interrupt` are preserved.
  pub fn flatten(self) -> Cause<E> {
    match self {
      Cause::Fail(inner) => inner,
      Cause::Die(msg) => Cause::Die(msg),
      Cause::Interrupt(id) => Cause::Interrupt(id),
      Cause::Both(l, r) => Cause::both(l.flatten(), r.flatten()),
      Cause::Then(l, r) => Cause::then(l.flatten(), r.flatten()),
    }
  }
}

/// `Cause<E>` with [`Cause::both`] forms a **commutative semigroup**.
///
/// Law: `combine(combine(a, b), c) ≡ combine(a, combine(b, c))`  (Both is associative).
impl<E: Clone + 'static> Semigroup for Cause<E> {
  #[inline]
  fn combine(self, other: Self) -> Self {
    Cause::both(self, other)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod constructors {
    use super::*;

    #[test]
    fn fail_with_error_returns_fail_variant() {
      assert_eq!(Cause::fail("boom"), Cause::Fail("boom"));
    }

    #[test]
    fn die_with_message_returns_die_variant() {
      assert_eq!(
        Cause::<&'static str>::die("defect"),
        Cause::Die("defect".into())
      );
    }

    #[test]
    fn interrupt_with_fiber_id_returns_interrupt_variant() {
      let fiber_id = FiberId::fresh();
      assert_eq!(
        Cause::<&'static str>::interrupt(fiber_id),
        Cause::Interrupt(fiber_id)
      );
    }

    #[test]
    fn both_with_two_causes_returns_both_variant() {
      let cause = Cause::both(Cause::fail("left"), Cause::die("right"));
      assert_eq!(
        cause,
        Cause::Both(Box::new(Cause::fail("left")), Box::new(Cause::die("right")))
      );
    }

    #[test]
    fn then_with_two_causes_returns_then_variant() {
      let cause = Cause::then(Cause::fail("left"), Cause::die("right"));
      assert_eq!(
        cause,
        Cause::Then(Box::new(Cause::fail("left")), Box::new(Cause::die("right")))
      );
    }
  }

  mod pretty {
    use super::*;

    #[test]
    fn cause_pretty_fail_variant() {
      assert_eq!(Cause::fail("boom").pretty(), "Fail(boom)");
    }

    #[test]
    fn cause_pretty_die_variant() {
      assert_eq!(Cause::<&str>::die("defect").pretty(), "Die(defect)");
    }

    #[rstest]
    #[case::fail(Cause::fail("boom"), "Fail(boom)")]
    #[case::die(Cause::die("defect"), "Die(defect)")]
    fn pretty_with_leaf_variants_renders_expected_text(
      #[case] cause: Cause<&'static str>,
      #[case] expected: &str,
    ) {
      assert_eq!(cause.pretty(), expected);
    }

    #[test]
    fn pretty_with_interrupt_variant_renders_fiber_identifier() {
      let fiber_id = FiberId::fresh();
      let cause = Cause::<&'static str>::interrupt(fiber_id);
      assert_eq!(cause.pretty(), format!("Interrupt({fiber_id})"));
    }

    #[test]
    fn pretty_with_both_variant_renders_left_and_right_causes() {
      let cause = Cause::both(Cause::fail("boom"), Cause::die("defect"));
      assert_eq!(cause.pretty(), "Both(Fail(boom), Die(defect))");
    }

    #[test]
    fn pretty_with_then_variant_renders_left_and_right_causes() {
      let cause = Cause::then(Cause::fail("boom"), Cause::die("defect"));
      assert_eq!(cause.pretty(), "Then(Fail(boom), Die(defect))");
    }
  }

  mod map_fail {
    use super::*;

    #[test]
    fn map_fail_with_fail_variant_transforms_error_type() {
      let mapped = Cause::fail(3u8).map_fail(|n| n.to_string());
      assert_eq!(mapped, Cause::fail(String::from("3")));
    }

    #[test]
    fn map_fail_with_die_variant_preserves_defect_message() {
      let mapped = Cause::<u8>::die("fatal").map_fail(|n| n.to_string());
      assert_eq!(mapped, Cause::die("fatal"));
    }

    #[test]
    fn map_fail_with_interrupt_variant_preserves_fiber_id() {
      let fiber_id = FiberId::fresh();
      let mapped = Cause::<u8>::interrupt(fiber_id).map_fail(|n| n.to_string());
      assert_eq!(mapped, Cause::interrupt(fiber_id));
    }

    #[test]
    fn map_fail_with_both_variant_maps_failures_recursively() {
      let source = Cause::both(Cause::fail(3u8), Cause::die("fatal"));
      let mapped = source.map_fail(|n| n.to_string());
      assert_eq!(
        mapped,
        Cause::both(Cause::fail(String::from("3")), Cause::die("fatal"))
      );
    }

    #[test]
    fn map_fail_with_then_variant_maps_failures_recursively() {
      let source = Cause::then(Cause::fail(7u8), Cause::interrupt(FiberId::ROOT));
      let mapped = source.map_fail(|n| n.to_string());
      assert_eq!(
        mapped,
        Cause::then(
          Cause::fail(String::from("7")),
          Cause::interrupt(FiberId::ROOT)
        )
      );
    }
  }

  mod effect_data {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn cause_eq_by_structural_value() {
      let a = Cause::both(Cause::fail("x"), Cause::die("d"));
      let b = Cause::both(Cause::fail("x"), Cause::die("d"));
      assert_eq!(a, b);
      let mut set = HashSet::new();
      set.insert(a.clone());
      assert!(set.contains(&b));
    }
  }

  mod fail_option {
    use super::*;

    #[test]
    fn fail_option_with_fail_variant_returns_some_error() {
      assert_eq!(Cause::fail("boom").fail_option(), Some("boom"));
    }

    #[test]
    fn fail_option_with_die_variant_returns_none() {
      assert_eq!(Cause::<u8>::die("x").fail_option(), None);
    }

    #[test]
    fn fail_option_with_interrupt_variant_returns_none() {
      assert_eq!(Cause::<u8>::interrupt(FiberId::fresh()).fail_option(), None);
    }

    #[test]
    fn fail_option_with_composite_variant_returns_none() {
      assert_eq!(
        Cause::both(Cause::fail(1u8), Cause::die("d")).fail_option(),
        None
      );
    }
  }

  mod contains {
    use super::*;

    #[test]
    fn contains_when_predicate_matches_root_returns_true() {
      let cause = Cause::fail("boom");
      assert!(cause.contains(&|c| matches!(c, Cause::Fail(_))));
    }

    #[test]
    fn contains_when_predicate_does_not_match_returns_false() {
      let cause = Cause::fail("boom");
      assert!(!cause.contains(&|c| matches!(c, Cause::Die(_))));
    }

    #[test]
    fn contains_when_predicate_matches_left_child_of_both_returns_true() {
      let cause = Cause::both(Cause::fail("x"), Cause::die("d"));
      assert!(cause.contains(&|c| matches!(c, Cause::Fail(_))));
    }

    #[test]
    fn contains_when_predicate_matches_right_child_of_both_returns_true() {
      let cause = Cause::both(Cause::die("d"), Cause::fail("x"));
      assert!(cause.contains(&|c| matches!(c, Cause::Fail(_))));
    }

    #[test]
    fn contains_when_predicate_matches_nested_then_child_returns_true() {
      let cause = Cause::then(
        Cause::<&str>::die("d"),
        Cause::then(Cause::<&str>::interrupt(FiberId::ROOT), Cause::fail("deep")),
      );
      assert!(cause.contains(&|c| matches!(c, Cause::Fail(_))));
    }

    #[test]
    fn contains_when_predicate_never_matches_returns_false() {
      let cause = Cause::both(Cause::<&str>::die("d"), Cause::interrupt(FiberId::ROOT));
      assert!(!cause.contains(&|c| matches!(c, Cause::Fail(_))));
    }
  }

  mod flatten {
    use super::*;

    #[test]
    fn flatten_with_fail_wrapping_inner_cause_unwraps_inner() {
      let inner: Cause<u8> = Cause::fail(42u8);
      let nested: Cause<Cause<u8>> = Cause::fail(inner.clone());
      assert_eq!(nested.flatten(), inner);
    }

    #[test]
    fn flatten_with_die_wrapping_cause_preserves_die_message() {
      let nested: Cause<Cause<u8>> = Cause::die("fatal");
      assert_eq!(nested.flatten(), Cause::<u8>::die("fatal"));
    }

    #[test]
    fn flatten_with_interrupt_preserves_fiber_id() {
      let id = FiberId::fresh();
      let nested: Cause<Cause<u8>> = Cause::interrupt(id);
      assert_eq!(nested.flatten(), Cause::<u8>::interrupt(id));
    }

    #[test]
    fn flatten_with_both_recursively_flattens_children() {
      let nested: Cause<Cause<u8>> =
        Cause::both(Cause::fail(Cause::fail(1u8)), Cause::fail(Cause::fail(2u8)));
      assert_eq!(
        nested.flatten(),
        Cause::both(Cause::fail(1u8), Cause::fail(2u8))
      );
    }

    #[test]
    fn flatten_with_then_recursively_flattens_children() {
      let nested: Cause<Cause<u8>> = Cause::then(Cause::fail(Cause::fail(3u8)), Cause::die("d"));
      assert_eq!(
        nested.flatten(),
        Cause::then(Cause::fail(3u8), Cause::<u8>::die("d"))
      );
    }
  }

  mod semigroup {
    use super::*;
    use crate::algebra::semigroup::Semigroup;

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
      let _ = (lhs, rhs);
    }
  }
}
