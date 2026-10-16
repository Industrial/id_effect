//! Ex 110 — Property testing (feature `proptest`): schema-derived generators, an effect-aware
//! property under `TestExecutor` virtual time, a model-based wallet test, `Exit` assertions.
//!
//! ```bash
//! cargo run -p id_effect --features proptest --example 110_property_testing
//! ```
use std::time::Duration;

use id_effect::schema::{HasSchema, ParseError, Schema, Unknown};
use id_effect::testing::property::{
  StateMachine, arb_duration_up_to, from_schema, prop_check, prop_sync, run_state_machine,
};
use id_effect::{
  Effect, Ref, assert_exit_fail, assert_exit_success, fail, render_exit, run_test, sleep, succeed,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};

/// Cents, never negative: the schema is the only way in.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cents(i64);

impl HasSchema for Cents {
  type A = Cents;
  type I = i64;
  type E = ();

  fn schema() -> Schema<Cents, i64, ()> {
    Schema::make(
      |n: i64| {
        if n >= 0 {
          Ok(Cents(n))
        } else {
          Err(ParseError::new("", "negative amount"))
        }
      },
      |c: Cents| c.0,
      |u: &Unknown| match u {
        Unknown::I64(n) if *n >= 0 => Ok(Cents(*n)),
        _ => Err(ParseError::new("", "expected a non-negative i64")),
      },
    )
  }
}

/// A slow pricing service: waits `latency`, then takes `percent` off.
fn discount(price: Cents, percent: i64, latency: Duration) -> Effect<Cents, TestCaseError> {
  sleep(latency).map(move |()| Cents(price.0 - price.0 * percent / 100))
}

/// Withdraws `amount` from `wallet`, failing on overdraft.
fn withdraw(wallet: &Ref<i64>, amount: i64) -> Effect<i64, String> {
  let w = wallet.clone();
  wallet.get().flat_map(move |balance| {
    if amount > balance {
      fail(format!("overdraft: {amount} > {balance}"))
    } else {
      w.update_and_get(move |b| b - amount)
    }
  })
}

#[derive(Clone, Debug)]
enum Op {
  Deposit(i64),
  Withdraw(i64),
}

/// The wallet against an `i64` model; withdrawals beyond the balance are skipped.
struct Wallet;

impl StateMachine for Wallet {
  type State = i64;
  type Sut = Ref<i64>;
  type Command = Op;

  fn initial_state(&self) -> i64 {
    0
  }

  fn commands(&self) -> BoxedStrategy<Op> {
    prop_oneof![
      (1i64..100).prop_map(Op::Deposit),
      (1i64..100).prop_map(Op::Withdraw)
    ]
    .boxed()
  }

  fn precondition(&self, balance: &i64, op: &Op) -> bool {
    !matches!(op, Op::Withdraw(n) if n > balance)
  }

  fn next_state(&self, balance: &i64, op: &Op) -> i64 {
    match op {
      Op::Deposit(n) => balance + n,
      Op::Withdraw(n) => balance - n,
    }
  }

  fn init_sut(&self) -> Effect<Ref<i64>, TestCaseError> {
    succeed(Ref::new(0))
  }

  fn apply(&self, wallet: &Ref<i64>, op: &Op, _: &i64, after: &i64) -> Effect<(), TestCaseError> {
    let expected = *after;
    let step = match op {
      Op::Deposit(n) => {
        let n = *n;
        wallet.update_and_get(move |b| b + n)
      }
      Op::Withdraw(n) => withdraw(wallet, *n).map_err(TestCaseError::fail),
    };
    step.flat_map(move |balance| {
      prop_sync(move || {
        prop_assert_eq!(balance, expected);
        Ok(())
      })
    })
  }
}

/// `cases` cases; no failure persistence (an example binary has no source tree to write
/// `proptest-regressions/` into; tests keep proptest's default).
fn config(cases: u32) -> Config {
  Config {
    failure_persistence: None,
    ..Config::with_cases(cases)
  }
}

fn main() {
  // 1. Generators derived from a schema, an effect run in virtual time per case.
  let mut runner = TestRunner::new(config(200));
  let prices = from_schema(Cents::schema(), 0i64..1_000_000);
  let cases = (
    prices,
    0i64..=100,
    arb_duration_up_to(Duration::from_secs(30)),
  );
  runner
    .run(&cases, |(price, percent, latency)| {
      let after = prop_check(discount(price, percent, latency))?;
      prop_assert!(after.0 >= 0 && after.0 <= price.0, "{price:?} -> {after:?}");
      Ok(())
    })
    .expect("a discount never goes negative or up");
  println!("discount property: 200 cases, up to 30 s of virtual latency each");

  // 2. Model-based: random deposit/withdraw sequences against the model.
  run_state_machine(&Wallet, config(64), 0..30);
  println!("wallet state machine: 64 command sequences");

  // 3. Exit assertions and a stable rendering for golden files.
  let wallet = Ref::new(10);
  assert_exit_success(&run_test(withdraw(&wallet, 4)), &6);
  let overdraft = run_test(withdraw(&wallet, 50));
  assert_exit_fail(&overdraft, &"overdraft: 50 > 6".to_string());
  println!("{}", render_exit(&overdraft));
  println!("110_property_testing ok");
}
