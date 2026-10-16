//! Integration tests for the first-party test helpers: `run_test`, the `Exit` / `Cause`
//! assertions, golden files, and (feature `proptest`) `testing::property`.

use std::time::Duration;

use id_effect::{
  Cause, Effect, Exit, ExitKind, Never, Schedule, assert_exit_die, assert_exit_eq,
  assert_exit_fail, assert_exit_kind, assert_exit_success, assert_golden, assert_golden_exit, fail,
  render_exit, run_test, sleep, succeed, sync,
};

/// Fails twice, then succeeds: retried with a 10 s spaced schedule in virtual time.
fn flaky(calls: std::sync::Arc<std::sync::atomic::AtomicU32>) -> Effect<u32, String> {
  sync(move || calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst)).flat_map(|n| {
    if n < 2 {
      fail(format!("attempt {n}"))
    } else {
      succeed(n)
    }
  })
}

#[test]
fn run_test_drives_a_retry_schedule_in_virtual_time() {
  let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
  let c = calls.clone();
  let exec = id_effect::TestExecutor::new();
  let start = id_effect::Executor::now(&exec);
  let eff = Effect::retry(
    move || flaky(c.clone()),
    Schedule::spaced(Duration::from_secs(10)).intersect(Schedule::recurs(5)),
  );
  assert_exit_success(&id_effect::run_test_on(&exec, eff), &2);
  assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
  assert_eq!(
    id_effect::Executor::now(&exec) - start,
    Duration::from_secs(20)
  );
}

#[test]
fn assertions_cover_each_exit_shape() {
  assert_exit_fail(&run_test(fail::<u8, _, _>("no")), &"no");
  assert_exit_die(&run_test(sync::<u8, Never, _>(|| panic!("oops"))), "oops");
  let slow: Exit<u8, Never> = run_test(sleep(Duration::from_secs(5)).map(|()| 1u8)).map(|n| n + 1);
  assert_exit_kind(&slow, ExitKind::Success);
  assert_exit_eq(
    &Exit::<(), &str>::Failure(Cause::both(Cause::fail("a"), Cause::die("b"))),
    &Exit::Failure(Cause::both(Cause::fail("a"), Cause::die("b"))),
  );
}

#[test]
fn golden_files_pin_rendered_exits() {
  let exit = run_test(fail::<u8, _, _>("boom").ensuring(succeed(())));
  assert_golden_exit("tests/golden/testing_property_exit.txt", &exit);
  assert_golden(
    "tests/golden/testing_property_text.txt",
    &format!(
      "{}\n",
      render_exit(&Exit::<Vec<u8>, ()>::succeed(vec![1, 2]))
    ),
  );
}

#[cfg(feature = "proptest")]
mod property {
  use std::collections::BTreeMap;

  use id_effect::schema::{HasSchema, ParseError, Schema, Unknown, i64, refine};
  use id_effect::testing::property::{
    StateMachine, arb_cause, arb_chunk, arb_duration, arb_exit, from_schema, prop_assert_exit_eq,
    prop_assert_roundtrip, prop_check, prop_run, prop_sync, run_state_machine, unknown_object,
  };
  use id_effect::{Chunk, Effect, Exit, Ref, Schema as DeriveSchema, Stream, sleep, succeed};
  use proptest::prelude::*;
  use proptest::test_runner::Config;

  #[derive(Clone, Debug, PartialEq, DeriveSchema)]
  struct User {
    #[schema("user_name")]
    name: String,
    age: i64,
  }

  fn user_wire() -> impl Strategy<Value = Unknown> {
    unknown_object([
      ("user_name", "[a-z]{1,8}".prop_map(Unknown::String).boxed()),
      ("age", (0..130i64).prop_map(Unknown::I64).boxed()),
    ])
  }

  struct Even;

  impl HasSchema for Even {
    type A = i64;
    type I = i64;
    type E = ();

    fn schema() -> Schema<i64, i64, ()> {
      refine(i64::<()>(), |n| n % 2 == 0, "odd")
    }
  }

  proptest! {
    #[test]
    fn derived_schema_values_roundtrip(user in from_schema(User::schema(), user_wire())) {
      prop_assert!(!user.name.is_empty());
      prop_assert_roundtrip(&<User as HasSchema>::schema(), user)?;
    }

    #[test]
    fn has_schema_generator_respects_the_refinement(n in from_schema(Even::schema(), -500i64..500)) {
      prop_assert_eq!(n % 2, 0);
    }

    #[test]
    fn stream_from_chunk_preserves_elements(chunk in arb_chunk(any::<u16>(), 0..64)) {
      let expected = chunk.clone().into_vec();
      let got: Vec<u16> = prop_run(Stream::<u16, ()>::from_iterable(chunk.into_vec()).run_collect())?;
      prop_assert_eq!(got, expected);
    }

    #[test]
    fn sleeps_are_virtual(d in arb_duration(), n in any::<u8>()) {
      let eff: Effect<u8, TestCaseError> = sleep(d).map(move |()| n);
      prop_assert_eq!(prop_check(eff)?, n);
    }

    #[test]
    fn exit_map_identity(exit in arb_exit(any::<u8>(), any::<u8>())) {
      prop_assert_exit_eq(&exit.clone().map(|a| a), &exit)?;
    }

    #[test]
    fn cause_map_fail_identity(cause in arb_cause(any::<i32>())) {
      prop_assert_eq!(cause.clone().map_fail(|e| e), cause);
    }
  }

  #[derive(Clone, Debug)]
  enum Op {
    Insert(u8, u8),
    Get(u8),
  }

  /// A `Ref<BTreeMap>` store against a `BTreeMap` model.
  struct Store;

  impl StateMachine for Store {
    type State = BTreeMap<u8, u8>;
    type Sut = Ref<BTreeMap<u8, u8>>;
    type Command = Op;

    fn initial_state(&self) -> BTreeMap<u8, u8> {
      BTreeMap::new()
    }

    fn commands(&self) -> BoxedStrategy<Op> {
      prop_oneof![
        (0u8..8, any::<u8>()).prop_map(|(k, v)| Op::Insert(k, v)),
        (0u8..8).prop_map(Op::Get),
      ]
      .boxed()
    }

    fn next_state(&self, state: &BTreeMap<u8, u8>, op: &Op) -> BTreeMap<u8, u8> {
      let mut s = state.clone();
      if let Op::Insert(k, v) = op {
        s.insert(*k, *v);
      }
      s
    }

    fn init_sut(&self) -> Effect<Ref<BTreeMap<u8, u8>>, TestCaseError> {
      succeed(Ref::new(BTreeMap::new()))
    }

    fn apply(
      &self,
      store: &Ref<BTreeMap<u8, u8>>,
      op: &Op,
      before: &BTreeMap<u8, u8>,
      _after: &BTreeMap<u8, u8>,
    ) -> Effect<(), TestCaseError> {
      match op.clone() {
        Op::Insert(k, v) => store.update(move |mut m| {
          m.insert(k, v);
          m
        }),
        Op::Get(k) => {
          let expected = before.get(&k).copied();
          store.get().flat_map(move |m| {
            prop_sync(move || {
              prop_assert_eq!(m.get(&k).copied(), expected);
              Ok(())
            })
          })
        }
      }
    }
  }

  #[test]
  fn store_matches_its_model() {
    run_state_machine(&Store, Config::with_cases(64), 0..40);
  }

  #[test]
  fn decode_errors_are_typed() {
    let bad = Unknown::Object(BTreeMap::new());
    let err: ParseError = User::schema().decode_unknown(&bad).unwrap_err();
    assert!(err.message.contains("missing"), "{err:?}");
    assert!(matches!(
      Exit::<Chunk<u8>, ()>::succeed(Chunk::empty()),
      Exit::Success(_)
    ));
  }
}
