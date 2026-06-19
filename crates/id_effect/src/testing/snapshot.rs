//! Snapshot assertions for Phase 0 behavior contracts.
//!
//! Snapshots pin deterministic outputs for core combinators so later parity
//! phases can evolve internals without silently drifting behavior.
//!
//! [`SnapshotAssertion`] comparison uses [`crate::schema::equal::equals`] (the [`crate::Equal`] protocol).
//! For map/set keys or explicit “data” typing, prefer also implementing
//! [`crate::schema::data::EffectData`] (typically via `#[derive(id_effect::EffectData)]` plus `Hash`).

use crate::capability::build_env;
use crate::kernel::{Effect, fail, pure, succeed};
use crate::provide;
use crate::scheduling::duration::duration;
use crate::scheduling::schedule::Schedule;
use crate::schema::equal::equals;
use crate::streaming::stream::Stream;

/// Deterministic snapshot record for baseline contract checks.
#[derive(Clone, Debug, crate::EffectData)]
pub struct SnapshotAssertion {
  /// Canonical snapshot id (matches [`SNAPSHOT_CORPUS`] entries).
  pub name: &'static str,
  /// Rendered output from the effect under test.
  pub observed: String,
  /// Frozen expected string for [`SnapshotAssertion::matches`].
  pub expected: &'static str,
}

impl SnapshotAssertion {
  /// Whether `observed` equals `expected` per [`crate::schema::equal::equals`] / [`crate::Equal`].
  #[inline]
  pub fn matches(&self) -> bool {
    equals(self.observed.as_str(), self.expected)
  }

  /// Panics unless this assertion structurally equals `expected` per [`crate::Equal`].
  #[inline]
  pub fn assert_equal(&self, expected: &SnapshotAssertion) {
    assert!(
      equals(self, expected),
      "snapshot assertion mismatch:\n  left: {self:?}\n right: {expected:?}"
    );
  }
}

/// Canonical snapshot names expected by the Phase 0 snapshot contract.
///
/// For each name, the paired [`SnapshotAssertion`] compares **observed** output to the frozen
/// **expected** string using the [`crate::Equal`] protocol via [`crate::schema::equal::equals`] in
/// [`SnapshotAssertion::matches`] (not raw `==` on [`String`]/`str`), so equality follows the
/// same structural rules as other `Equal` witnesses in this crate.
pub const SNAPSHOT_CORPUS: [&str; 6] = [
  "snapshot_effect_map_flat_map",
  "snapshot_effect_catch_map_error",
  "snapshot_capability_env_lookup",
  "snapshot_schedule_recurs_exponential",
  "snapshot_stream_map_filter_grouped",
  "snapshot_scope_finalizer_order_placeholder",
];

/// Snapshots map/flat_map value propagation.
pub fn snapshot_effect_map_flat_map() -> Effect<SnapshotAssertion, (), ()> {
  pure::<i32>(2)
    .map(|n| n + 1)
    .flat_map(|n| succeed::<i32, (), ()>(n * 3))
    .map(|value| SnapshotAssertion {
      name: "snapshot_effect_map_flat_map",
      observed: value.to_string(),
      expected: "9",
    })
}

/// Snapshots typed error mapping and recovery through `catch`.
pub fn snapshot_effect_catch_map_error() -> Effect<SnapshotAssertion, (), ()> {
  fail::<u8, &'static str, ()>("boom")
    .map_error(|_| ())
    .catch(|_| succeed::<u8, (), ()>(5))
    .map(|value| SnapshotAssertion {
      name: "snapshot_effect_catch_map_error",
      observed: value.to_string(),
      expected: "5",
    })
}

/// Snapshot corpus db capability key.
#[allow(missing_docs)]
#[allow(dead_code)]
#[::id_effect::capability(i32)]
struct SnapshotDb;

/// Snapshot corpus clock capability key.
#[allow(missing_docs)]
#[allow(dead_code)]
#[::id_effect::capability(u64)]
struct SnapshotClock;

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(SnapshotDbKey)]
struct SnapshotDbLive;

#[allow(clippy::new_ret_no_self)]
impl SnapshotDbLive {
  fn new() -> i32 {
    7
  }
}

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(SnapshotClockKey)]
struct SnapshotClockLive;

#[allow(clippy::new_ret_no_self)]
impl SnapshotClockLive {
  fn new() -> u64 {
    11
  }
}

/// Snapshots capability env build + typed lookup as merge/provide baseline.
pub fn snapshot_capability_env_lookup() -> Effect<SnapshotAssertion, (), ()> {
  Effect::new(|_| {
    let env = build_env([provide!(SnapshotDbLive), provide!(SnapshotClockLive)]).map_err(|_| ())?;
    let got_db = *env.get::<SnapshotDbKey>();
    let got_clock = *env.get::<SnapshotClockKey>();
    Ok(SnapshotAssertion {
      name: "snapshot_capability_env_lookup",
      observed: format!("{got_db}:{got_clock}"),
      expected: "7:11",
    })
  })
}

/// Snapshots schedule constructor semantics via deterministic debug shape.
pub fn snapshot_schedule_recurs_exponential() -> Effect<SnapshotAssertion, (), ()> {
  let schedule = Schedule::recurs(2).compose(Schedule::exponential(duration::millis(5)).jittered());
  succeed::<SnapshotAssertion, (), ()>(SnapshotAssertion {
    name: "snapshot_schedule_recurs_exponential",
    observed: format!("{schedule:?}"),
    expected: "Compose(Recurs { remaining: 2 }, Jittered(Exponential { base: 5ms, step: 0 }))",
  })
}

/// Snapshots stream transformation and grouping output shape.
pub fn snapshot_stream_map_filter_grouped() -> Effect<SnapshotAssertion, (), ()> {
  Stream::from_iterable(1..=8)
    .map(|n| n * 2)
    .filter(Box::new(|n: &i32| *n % 4 == 0))
    .grouped(2)
    .run_collect()
    .map(|chunks| SnapshotAssertion {
      name: "snapshot_stream_map_filter_grouped",
      observed: format!("{chunks:?}"),
      expected: "[[4, 8], [12, 16]]",
    })
}

/// Placeholder snapshot until scoped finalizer ordering is implemented.
pub fn snapshot_scope_finalizer_order_placeholder() -> Effect<SnapshotAssertion, (), ()> {
  succeed::<SnapshotAssertion, (), ()>(SnapshotAssertion {
    name: "snapshot_scope_finalizer_order_placeholder",
    observed: "placeholder:lifo-finalizer-order-pending".to_owned(),
    expected: "placeholder:lifo-finalizer-order-pending",
  })
}

/// Returns all phase-0 snapshot assertions in canonical order.
pub fn snapshot_suite() -> [Effect<SnapshotAssertion, (), ()>; 6] {
  [
    snapshot_effect_map_flat_map(),
    snapshot_effect_catch_map_error(),
    snapshot_capability_env_lookup(),
    snapshot_schedule_recurs_exponential(),
    snapshot_stream_map_filter_grouped(),
    snapshot_scope_finalizer_order_placeholder(),
  ]
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod snapshot_assertion {
    use super::*;

    #[rstest]
    #[case::exact_match("value", "value", true)]
    #[case::different_value("observed", "expected", false)]
    fn matches_with_observed_and_expected_reports_contract_match(
      #[case] observed: &'static str,
      #[case] expected: &'static str,
      #[case] should_match: bool,
    ) {
      let assertion = SnapshotAssertion {
        name: "test",
        observed: observed.to_owned(),
        expected,
      };
      assert_eq!(assertion.matches(), should_match);
    }

    mod assert_equal {
      use super::*;

      #[test]
      fn assertion_passes_when_equal() {
        let a = SnapshotAssertion {
          name: "t",
          observed: "x".into(),
          expected: "x",
        };
        let b = SnapshotAssertion {
          name: "t",
          observed: "x".into(),
          expected: "x",
        };
        a.assert_equal(&b);
      }

      #[test]
      #[should_panic(expected = "snapshot assertion mismatch")]
      fn assertion_fails_when_unequal() {
        let a = SnapshotAssertion {
          name: "t",
          observed: "1".into(),
          expected: "1",
        };
        let b = SnapshotAssertion {
          name: "t",
          observed: "2".into(),
          expected: "2",
        };
        a.assert_equal(&b);
      }
    }

    #[test]
    fn snapshot_assertion_usable_in_hashset() {
      use std::collections::HashSet;

      let a = SnapshotAssertion {
        name: "n",
        observed: "o".into(),
        expected: "e",
      };
      let b = SnapshotAssertion {
        name: "n",
        observed: "o".into(),
        expected: "e",
      };
      let mut set = HashSet::new();
      set.insert(a);
      assert!(set.contains(&b));
    }

    #[test]
    fn effect_map_flat_map_snapshot_regression() {
      let expected = SnapshotAssertion {
        name: "snapshot_effect_map_flat_map",
        observed: "9".into(),
        expected: "9",
      };
      let got =
        pollster::block_on(snapshot_effect_map_flat_map().run(&mut ())).expect("snapshot ok");
      got.assert_equal(&expected);
    }
  }

  mod corpus {
    use super::*;

    #[test]
    fn snapshot_corpus_contains_phase_zero_snapshot_names_in_canonical_order() {
      assert_eq!(
        SNAPSHOT_CORPUS,
        [
          "snapshot_effect_map_flat_map",
          "snapshot_effect_catch_map_error",
          "snapshot_capability_env_lookup",
          "snapshot_schedule_recurs_exponential",
          "snapshot_stream_map_filter_grouped",
          "snapshot_scope_finalizer_order_placeholder",
        ]
      );
    }
  }

  mod golden_helpers {
    use super::*;

    #[test]
    fn golden_builder_assert_observed_passes_on_match() {
      GoldenBuilder::new("case", "9").assert_observed("9");
    }

    #[test]
    fn assert_golden_effect_runs_snapshot_contract() {
      let snapshot = assert_golden_effect(snapshot_effect_map_flat_map(), ());
      assert_eq!(snapshot.name, "snapshot_effect_map_flat_map");
    }
  }

  mod snapshot_suite_contract {
    use super::*;

    #[test]
    fn snapshot_suite_with_phase_zero_effects_matches_expected_contract() {
      let suite = snapshot_suite();
      assert_eq!(suite.len(), SNAPSHOT_CORPUS.len());

      for (idx, effect) in suite.into_iter().enumerate() {
        let out = pollster::block_on(effect.run(&mut ()));
        let snapshot = out.expect("snapshot effect failed unexpectedly");
        assert_eq!(snapshot.name, SNAPSHOT_CORPUS[idx]);
        assert!(
          snapshot.matches(),
          "snapshot mismatch: {} observed={} expected={}",
          snapshot.name,
          snapshot.observed,
          snapshot.expected
        );
      }
    }

    #[rstest]
    #[case::effect_map_flat_map(snapshot_effect_map_flat_map(), "snapshot_effect_map_flat_map")]
    #[case::effect_catch_map_error(
      snapshot_effect_catch_map_error(),
      "snapshot_effect_catch_map_error"
    )]
    #[case::capability_env_lookup(
      snapshot_capability_env_lookup(),
      "snapshot_capability_env_lookup"
    )]
    #[case::schedule_recurs_exponential(
      snapshot_schedule_recurs_exponential(),
      "snapshot_schedule_recurs_exponential"
    )]
    #[case::stream_map_filter_grouped(
      snapshot_stream_map_filter_grouped(),
      "snapshot_stream_map_filter_grouped"
    )]
    #[case::scope_placeholder(
      snapshot_scope_finalizer_order_placeholder(),
      "snapshot_scope_finalizer_order_placeholder"
    )]
    fn snapshot_effect_with_known_name_produces_matching_assertion(
      #[case] effect: Effect<SnapshotAssertion, (), ()>,
      #[case] expected_name: &'static str,
    ) {
      let snapshot = pollster::block_on(effect.run(&mut ())).expect("snapshot should succeed");
      assert_eq!(snapshot.name, expected_name);
      assert!(snapshot.matches());
    }
  }
}

/// Builder for golden snapshot assertions with fluent expected-value wiring.
#[derive(Clone, Debug)]
pub struct GoldenBuilder {
  name: &'static str,
  expected: &'static str,
}

impl GoldenBuilder {
  /// Start a golden assertion for `name`.
  #[inline]
  pub fn new(name: &'static str, expected: &'static str) -> Self {
    Self { name, expected }
  }

  /// Build a [`SnapshotAssertion`] from an observed string.
  #[inline]
  pub fn build(self, observed: impl Into<String>) -> SnapshotAssertion {
    SnapshotAssertion {
      name: self.name,
      observed: observed.into(),
      expected: self.expected,
    }
  }

  /// Assert `observed` matches the frozen expected value.
  #[inline]
  pub fn assert_observed(self, observed: impl Into<String>) {
    self.build(observed).assert_matches();
  }
}

impl SnapshotAssertion {
  /// Panics unless `observed` equals `expected` per [`crate::Equal`].
  #[inline]
  pub fn assert_matches(&self) {
    assert!(
      self.matches(),
      "golden snapshot mismatch for `{}`:\n  observed: {}\n  expected: {}",
      self.name,
      self.observed,
      self.expected
    );
  }

  /// Assert this snapshot's name is in [`SNAPSHOT_CORPUS`].
  #[inline]
  pub fn assert_corpus_member(&self) {
    assert!(
      SNAPSHOT_CORPUS.contains(&self.name),
      "snapshot `{}` is not listed in SNAPSHOT_CORPUS",
      self.name
    );
  }
}

/// Assert a pre-built snapshot matches its expected value.
#[inline]
pub fn assert_golden(snapshot: &SnapshotAssertion) {
  snapshot.assert_matches();
}

/// Assert `observed` matches a frozen golden string for `name`.
#[inline]
pub fn assert_golden_matches(
  name: &'static str,
  expected: &'static str,
  observed: impl Into<String>,
) {
  GoldenBuilder::new(name, expected).assert_observed(observed);
}

/// Run an effect and assert the resulting snapshot matches its expected value.
pub fn assert_golden_effect<E, R>(
  effect: Effect<SnapshotAssertion, E, R>,
  env: R,
) -> SnapshotAssertion
where
  E: std::fmt::Debug + 'static,
  R: 'static,
{
  let snapshot = crate::runtime::run_blocking(effect, env).expect("golden effect failed");
  assert_golden(&snapshot);
  snapshot
}
