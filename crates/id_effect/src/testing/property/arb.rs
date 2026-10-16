//! Strategies for framework domain types: `Duration`, `Chunk`, `FiberId`, `Defect`, `Cause`,
//! `Exit` and capability HLists.

use std::fmt::Debug;
use std::time::Duration;

use proptest::collection::{SizeRange, vec};
use proptest::prelude::{Just, Strategy, any};
use proptest::prop_oneof;

use crate::{Cause, Chunk, Defect, Exit, FiberId, HCons, HNil};

/// Durations spread over the scales effects use: zero, sub-millisecond, sub-second, minutes
/// and up to one day (nanosecond resolution). Shrinks towards zero.
pub fn arb_duration() -> impl Strategy<Value = Duration> {
  prop_oneof![
    Just(Duration::ZERO),
    arb_duration_up_to(Duration::from_millis(1)),
    arb_duration_up_to(Duration::from_secs(1)),
    arb_duration_up_to(Duration::from_secs(3600)),
    arb_duration_up_to(Duration::from_secs(86_400)),
  ]
}

/// Durations uniform in `0..=max` (nanosecond resolution, `max` capped at `u64::MAX` ns).
/// Shrinks towards zero.
pub fn arb_duration_up_to(max: Duration) -> impl Strategy<Value = Duration> {
  let max = u64::try_from(max.as_nanos()).unwrap_or(u64::MAX);
  (0..=max).prop_map(Duration::from_nanos)
}

/// A [`Chunk`] of `size` elements drawn from `element`. Shrinks by dropping and shrinking
/// elements.
pub fn arb_chunk<S: Strategy>(
  element: S,
  size: impl Into<SizeRange>,
) -> impl Strategy<Value = Chunk<S::Value>> {
  vec(element, size).prop_map(Chunk::from_vec)
}

/// Any [`FiberId`].
pub fn arb_fiber_id() -> impl Strategy<Value = FiberId> {
  any::<u64>().prop_map(FiberId::new)
}

/// A [`Defect`] with a short printable message.
pub fn arb_defect() -> impl Strategy<Value = Defect> {
  "[a-z][a-z ]{0,15}".prop_map(Defect::new)
}

/// A [`Cause`] tree: leaves are `Fail(error)`, `Die` and `Interrupt`, combined with `Both` and
/// `Then` up to depth 3. Shrinks towards a single leaf.
pub fn arb_cause<S>(error: S) -> impl Strategy<Value = Cause<S::Value>>
where
  S: Strategy + 'static,
  S::Value: Clone + 'static,
{
  let leaf = prop_oneof![
    3 => error.prop_map(Cause::Fail),
    1 => arb_defect().prop_map(Cause::Die),
    1 => arb_fiber_id().prop_map(Cause::Interrupt),
  ];
  leaf.prop_recursive(3, 16, 2, |inner| {
    prop_oneof![
      (inner.clone(), inner.clone()).prop_map(|(l, r)| Cause::both(l, r)),
      (inner.clone(), inner).prop_map(|(l, r)| Cause::then(l, r)),
    ]
  })
}

/// An [`Exit`]: `Success(value)` or `Failure(cause)` with a cause from [`arb_cause`].
pub fn arb_exit<SA, SE>(value: SA, error: SE) -> impl Strategy<Value = Exit<SA::Value, SE::Value>>
where
  SA: Strategy,
  SE: Strategy + 'static,
  SE::Value: Clone + 'static,
{
  prop_oneof![
    value.prop_map(Exit::Success),
    arb_cause(error).prop_map(Exit::Failure),
  ]
}

/// `Exit::Success(value)` only.
pub fn arb_exit_success<S: Strategy, E: Debug>(
  value: S,
) -> impl Strategy<Value = Exit<S::Value, E>> {
  value.prop_map(Exit::Success)
}

/// `Exit::Failure(Cause::Fail(error))` only.
pub fn arb_exit_fail<A: Debug, S: Strategy>(error: S) -> impl Strategy<Value = Exit<A, S::Value>> {
  error.prop_map(Exit::fail)
}

/// The empty capability list.
pub fn arb_hnil() -> Just<HNil> {
  Just(HNil)
}

/// A capability list `HCons<H, T>`: a generated capability `head` in front of a generated
/// `tail`, ready for `Effect::provide_env` (e.g. a random config or a seeded double).
pub fn arb_hcons<SH, ST>(head: SH, tail: ST) -> impl Strategy<Value = HCons<SH::Value, ST::Value>>
where
  SH: Strategy,
  ST: Strategy,
{
  (head, tail).prop_map(|(h, t)| HCons::new(h, t))
}

#[cfg(test)]
mod tests {
  use proptest::prelude::*;

  use super::*;
  use crate::kernel::ExitKind;

  fn depth<E>(c: &Cause<E>) -> usize {
    match c {
      Cause::Both(l, r) | Cause::Then(l, r) => 1 + depth(l).max(depth(r)),
      _ => 0,
    }
  }

  proptest! {
    #[test]
    fn durations_stay_within_a_day(d in arb_duration()) {
      prop_assert!(d <= Duration::from_secs(86_400));
    }

    #[test]
    fn bounded_durations_respect_the_bound(d in arb_duration_up_to(Duration::from_millis(5))) {
      prop_assert!(d <= Duration::from_millis(5));
    }

    #[test]
    fn chunks_have_the_requested_size(c in arb_chunk(any::<u8>(), 2..5)) {
      prop_assert!((2..5).contains(&c.len()));
    }

    #[test]
    fn causes_are_bounded_trees(c in arb_cause(any::<u8>())) {
      prop_assert!(depth(&c) <= 3);
    }

    #[test]
    fn exits_classify(exit in arb_exit(any::<u8>(), any::<u8>())) {
      let kind = ExitKind::of(&exit);
      prop_assert_eq!(kind.is_success(), matches!(exit, Exit::Success(_)));
    }

    #[test]
    fn success_and_fail_exits_have_their_shape(
      ok in arb_exit_success::<_, ()>(any::<u8>()),
      err in arb_exit_fail::<(), _>(any::<u8>()),
    ) {
      prop_assert!(matches!(ok, Exit::Success(_)));
      prop_assert!(matches!(err, Exit::Failure(Cause::Fail(_))));
    }

    #[test]
    fn defects_have_a_message(d in arb_defect()) {
      prop_assert!(!d.message().is_empty());
    }

    #[test]
    fn hcons_carries_the_generated_head(
      env in arb_hcons(1u16..10, arb_hcons(Just(FiberId::new(7)), arb_hnil())),
    ) {
      let HCons(head, HCons(id, HNil)) = env;
      prop_assert!((1..10).contains(&*head));
      prop_assert_eq!(&*id, &FiberId::new(7));
    }
  }

  #[test]
  fn duration_bound_saturates_past_u64_nanos() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let tree = arb_duration_up_to(Duration::MAX)
      .new_tree(&mut TestRunner::deterministic())
      .unwrap();
    assert!(tree.current() <= Duration::from_nanos(u64::MAX));
  }

  #[test]
  fn arb_cause_reaches_every_leaf_kind() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let (mut fail, mut die, mut interrupt) = (false, false, false);
    for _ in 0..200 {
      let c = arb_cause(any::<u8>())
        .new_tree(&mut runner)
        .unwrap()
        .current();
      fail |= c.contains(&|c| matches!(c, Cause::Fail(_)));
      die |= c.contains(&|c| matches!(c, Cause::Die(_)));
      interrupt |= c.contains(&|c| matches!(c, Cause::Interrupt(_)));
    }
    assert!(fail && die && interrupt);
  }
}
