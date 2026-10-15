# Why STM? — The Shared State Problem

Consider transferring money between two accounts. With mutexes:

```rust,ignore
fn transfer(from: &Mutex<Account>, to: &Mutex<Account>, amount: u64) {
    let mut from_guard = from.lock().unwrap();
    // Thread B might be doing transfer(to, from, ...) right here
    let mut to_guard = to.lock().unwrap();
    // DEADLOCK: Thread A holds from, waiting for to.
    //           Thread B holds to, waiting for from.
    from_guard.balance -= amount;
    to_guard.balance += amount;
}
```

The standard fix (always lock in a consistent order) requires global coordination across your codebase. Add a third account and you need to sort three locks. It doesn't compose.

## STM: Optimistic Concurrency

STM assumes conflicts are rare. Instead of locking, a transaction:

1. reads current values into a private transaction log,
2. computes new values from those reads (writes stay in the log),
3. commits: checks that nothing it read has changed, then applies its writes atomically.

If something changed between step 1 and step 3, the transaction restarts from step 1.

```rust,ignore
use id_effect_fp::{Stm, TRef, commit};

fn transfer(from: &TRef<Account>, to: &TRef<Account>, amount: u64) -> Stm<(), TransferError> {
  let (from, to) = (from.clone(), to.clone());
  from.read_stm().flat_map(move |src: Account| {
    if src.balance < amount {
      return Stm::fail(TransferError::InsufficientFunds);
    }
    let (from, to) = (from.clone(), to.clone());
    to.read_stm().flat_map(move |dst: Account| {
      let debit = Account { balance: src.balance - amount, ..src.clone() };
      let credit = Account { balance: dst.balance + amount, ..dst };
      let to = to.clone();
      from.write_stm(debit).flat_map(move |()| to.write_stm(credit.clone()))
    })
  })
}

// Run it: `commit` lifts the transaction into an Effect.
run(commit(transfer(&a, &b, 1)))?;
```

No locks and no lock ordering. The runnable example starts two threads that make 50 transfers each
between the same two accounts in opposite directions, the case that deadlocks with two mutexes, and
asserts the balances at the end. A transfer that fails with `InsufficientFunds` writes nothing.

Full example: [`crates/id_effect_fp/examples/083_stm_transfer.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_fp/examples/083_stm_transfer.rs) (`cargo run -p id_effect_fp --example 083_stm_transfer`).

Transactions are plain values built with `flat_map`; there is no `stm!` do-notation macro. Clone the
`TRef` handles you need into each closure (they are cheap `Arc` clones). The closures are `Fn`
because a transaction may run more than once.

## When STM Wins

| Situation | Mutex | STM |
|-----------|-------|-----|
| Single shared value | ✓ simple | ✓ fine |
| Multiple related values | ✗ deadlock risk | ✓ composable |
| Composing two existing operations | ✗ requires coordination | ✓ `flat_map` them into one transaction |
| Long operations with I/O | ✓ (STM would restart too much) | ✗ wrong tool |

STM shines when:
- you need to update multiple values atomically,
- you are composing smaller transactional operations into larger ones,
- contention is low (restarts are cheap).

Avoid STM for long-running operations that do I/O. A transaction body may run several times, so it
must be short and free of side effects: read-modify-write only, no network calls.
