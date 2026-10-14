//! Ex 083 — STM bank transfer (book: "Why STM?").
//!
//! Two threads move money between the same two accounts in opposite directions. With two mutexes
//! that is the classic deadlock; with STM each transfer reads both cells, writes both, and commits
//! atomically, restarting if the other thread committed in between.
//!
//! Run: `cargo run -p id_effect_fp --example 083_stm_transfer`
use std::thread;

use id_effect::run_blocking;
use id_effect_fp::{Stm, TRef, commit};

#[derive(Clone, Debug, PartialEq)]
struct Account {
  id: u32,
  balance: u64,
}

#[derive(Clone, Debug, PartialEq)]
enum TransferError {
  InsufficientFunds,
}

/// Debit `from` and credit `to` in one transaction, or fail without writing anything.
fn transfer(from: &TRef<Account>, to: &TRef<Account>, amount: u64) -> Stm<(), TransferError> {
  let (from, to) = (from.clone(), to.clone());
  from.read_stm().flat_map(move |src: Account| {
    if src.balance < amount {
      return Stm::fail(TransferError::InsufficientFunds);
    }
    let (from, to) = (from.clone(), to.clone());
    to.read_stm().flat_map(move |dst: Account| {
      let debit = Account {
        balance: src.balance - amount,
        ..src.clone()
      };
      let credit = Account {
        balance: dst.balance + amount,
        ..dst
      };
      let to = to.clone();
      from
        .write_stm(debit)
        .flat_map(move |()| to.write_stm(credit.clone()))
    })
  })
}

/// Allocate a cell once, outside any transaction, so every thread shares it.
fn account(id: u32, balance: u64) -> TRef<Account> {
  run_blocking(commit(TRef::make(Account { id, balance })), ()).expect("cell")
}

fn balance(acct: &TRef<Account>) -> u64 {
  run_blocking(commit(acct.read_stm::<()>()), ())
    .expect("read")
    .balance
}

fn main() {
  let a = account(1, 100);
  let b = account(2, 50);

  // 50 transfers each way, concurrently: no locks to order, no deadlock.
  let handles: Vec<_> = [(a.clone(), b.clone()), (b.clone(), a.clone())]
    .into_iter()
    .map(|(from, to)| {
      thread::spawn(move || {
        for _ in 0..50 {
          run_blocking(commit::<_, _, ()>(transfer(&from, &to, 1)), ()).expect("transfer");
        }
      })
    })
    .collect();
  for h in handles {
    h.join().expect("join");
  }
  assert_eq!((balance(&a), balance(&b)), (100, 50));

  // A failed transaction writes nothing.
  let r = run_blocking(commit::<_, _, ()>(transfer(&b, &a, 1_000)), ());
  assert_eq!(r, Err(TransferError::InsufficientFunds));
  assert_eq!((balance(&a), balance(&b)), (100, 50));

  let ids = [a, b].map(|acct| {
    run_blocking(commit(acct.read_stm::<()>()), ())
      .expect("read")
      .id
  });
  println!("accounts {ids:?} balanced after 100 concurrent transfers");
  println!("083_stm_transfer ok");
}
