//! Ex 085 — `Stm::fail`, `Stm::check` / retry, `or_else` and composition (book: "Stm and commit").
//!
//! Run: `cargo run -p id_effect_fp --example 085_stm_fail_retry`
use std::thread;

use id_effect::next::Effect;
use id_effect::{Cause, Exit};
use id_effect_fp::{Stm, TRef, commit};

#[derive(Clone, Debug, PartialEq)]
struct InsufficientFunds;

/// Fails (no retry, no write) when the balance is too low.
fn withdraw(account: &TRef<u64>, amount: u64) -> Stm<u64, InsufficientFunds> {
  let acct = account.clone();
  account.read_stm().flat_map(move |balance| {
    if balance < amount {
      return Stm::fail(InsufficientFunds);
    }
    acct
      .write_stm(balance - amount)
      .map(move |()| balance - amount)
  })
}

/// Retries (restarts the transaction) until the balance covers `amount`.
fn withdraw_when_available(account: &TRef<u64>, amount: u64) -> Stm<u64, ()> {
  let acct = account.clone();
  account.read_stm().flat_map(move |balance| {
    let acct = acct.clone();
    Stm::<(), ()>::check(balance >= amount).flat_map(move |()| {
      acct
        .write_stm(balance - amount)
        .map(move |()| balance - amount)
    })
  })
}

fn deposit(account: &TRef<u64>, amount: u64) -> Stm<(), ()> {
  account.update_stm(move |b| b + amount)
}

fn main() {
  let account = run(commit(TRef::make(100_u64))).expect("cell");

  // commit lifts `Stm<A, E>` into `Effect<A, E, R>`; the error type comes from the transaction.
  let effect: Effect<u64, InsufficientFunds> = commit(withdraw(&account, 70));
  assert_eq!(run(effect), Ok(30));
  assert_eq!(run(commit(withdraw(&account, 70))), Err(InsufficientFunds));

  // `or_else` turns a would-block transaction into a non-blocking try.
  let try_now = withdraw_when_available(&account, 80).or_else(Stm::succeed(u64::MAX));
  assert_eq!(run(commit(try_now)), Ok(u64::MAX), "30 < 80: not now");

  // A waiter retries until another thread's deposit makes the condition true.
  let waiter = {
    let account = account.clone();
    thread::spawn(move || run(commit(withdraw_when_available(&account, 80))))
  };
  run(commit(deposit(&account, 50))).expect("deposit"); // 30 + 50
  assert_eq!(waiter.join().expect("join"), Ok(0));

  // Composition: a withdrawal and an audit counter commit together, or not at all.
  let audit = run(commit(TRef::make(0_u32))).expect("cell");
  run(commit(deposit(&account, 10))).expect("deposit");
  let audited = {
    let audit = audit.clone();
    withdraw(&account, 5).flat_map(move |left| audit.update_stm(|n| n + 1).map(move |()| left))
  };
  assert_eq!(run(commit(audited)), Ok(5));
  assert_eq!(run(commit(audit.read_stm::<()>())), Ok(1));
  println!("085_stm_fail_retry ok");
}

/// Runs `eff` on the global next runtime: `Ok` on success, `Err` on a typed failure.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
