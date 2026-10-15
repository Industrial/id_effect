# Property Testing — Testing Invariants

Unit tests check specific examples. Property tests check that an invariant holds for *all*
inputs. id_effect has no property-testing module of its own: use the
[`proptest`](https://docs.rs/proptest) crate as a dev-dependency and run each generated case with
`block_on`. Pure and fused synchronous effects settle inline without touching an executor, so
hundreds of cases run in milliseconds.

## Effect Laws

```rust,ignore
use id_effect::{Effect, Exit, succeed};
use proptest::prelude::*;

proptest! {
  #[test]
  fn zip_order_does_not_matter(a in any::<i32>(), b in any::<i32>()) {
    let ab: Effect<(i32, i32)> = succeed(a).zip(succeed(b));
    let ba: Effect<(i32, i32)> = succeed(b).zip(succeed(a)).map(|(x, y)| (y, x));
    prop_assert_eq!(ab.block_on(), ba.block_on());
  }

  #[test]
  fn map_fusion(n in any::<i64>()) {
    let f = |x: i64| x.wrapping_mul(3);
    let g = |x: i64| x.wrapping_add(1);
    let twice: Effect<i64> = succeed(n).map(f).map(g);
    let once: Effect<i64> = succeed(n).map(move |x| g(f(x)));
    prop_assert_eq!(twice.block_on(), once.block_on());
  }
}
```

Effects are one-shot values, so build a fresh effect for every comparison instead of reusing one.

## Codec Round-Trips

Every schema combinator is tested this way in `crates/id_effect/tests/schema_roundtrip.rs`:

```rust,ignore
use id_effect::schema::{array, i64, optional, string};

proptest! {
  #[test]
  fn array_roundtrip(v in prop::collection::vec(".*", 0..16)) {
    let s = array(string::<()>());
    prop_assert_eq!(s.decode(s.encode(v.clone())), Ok(v));
  }

  #[test]
  fn optional_roundtrip(o in proptest::option::of(any::<i64>())) {
    let s = optional(i64::<()>());
    prop_assert_eq!(s.decode(s.encode(o)), Ok(o));
  }
}
```

## Business Invariants with STM

```rust,ignore
use id_effect::{Cause, Exit};
use id_effect_fp::{TRef, commit};

proptest! {
  #[test]
  fn withdraw_never_overdraws(balance in 0_u64..10_000, amount in 0_u64..20_000) {
    let account = commit(TRef::make(balance)).block_on().into_result().expect("cell");
    let exit = commit(withdraw(&account, amount)).block_on();
    let after = commit(account.read_stm::<()>()).block_on().into_result().expect("read");

    match exit {
      Exit::Success(left) => prop_assert_eq!(after, left),
      Exit::Failure(Cause::Fail(InsufficientFunds)) => prop_assert_eq!(after, balance),
      Exit::Failure(other) => prop_assert!(false, "unexpected {other:?}"),
    }
    prop_assert!(after <= balance);
  }
}
```

(`withdraw` is the transaction from [Stm and commit](./ch12-03-stm-commit.md).)

## Services with Fakes

Provide a fake capability per case, exactly as in [Mocking](./ch15-03-mocking.md):

```rust,ignore
proptest! {
  #[test]
  fn save_then_find_returns_the_user(user in arb_user()) {
    let repo = Arc::new(InMemoryUsers::default());
    let id = user.id;
    let program = save_user(user.clone()).flat_map(move |()| find_user(id));
    let exit = program.provide(Users(repo)).block_on();
    prop_assert_eq!(exit.into_result().ok(), Some(Some(user)));
  }
}
```

## Randomness and Time

For code that draws random numbers through the `Random` capability, provide
`TestRandom::seeded(seed).random()` and let proptest pick the seed; `feed_u64s` / `feed_f64s`
queue exact values. For timing properties, generate durations and run the effect with the
`run_virtual` helper from [Virtual Time with TestExecutor](./ch15-02-test-clock.md), so even long
schedules stay fast.

## Shrinking

`proptest` shrinks failing inputs to the smallest case that still fails. Because these tests do no
real I/O and wait on no real timers, shrinking stays quick even over many iterations.
