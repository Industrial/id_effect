# Stm and commit — Building Transactions

An `Stm<A, E>` describes a transaction. Build one from `TRef` operations and the constructors
below, compose with `map` / `flat_map` / `or_else`, and execute it with `commit`.

| Constructor | Meaning |
|-------------|---------|
| `Stm::succeed(a)` | finish with `a` |
| `Stm::fail(e)` | abort with error `e`; nothing is written, no restart |
| `Stm::retry()` | restart the transaction |
| `Stm::check(cond)` | continue if `cond`, otherwise retry |

## commit: Lift Stm into Effect

```rust,ignore
use id_effect::{Effect, Exit};
use id_effect_fp::{Stm, TRef, commit};

let effect: Effect<u64, InsufficientFunds> = commit(withdraw(&account, 70));
assert_eq!(effect.block_on(), Exit::succeed(30));
```

`commit(stm)` returns an `Effect<A, E, R>`, generic over any environment `R`. Its error type is the
transaction's `E`. Running the effect executes the transaction; if commit-time validation finds a
cell it read has changed, it starts again. `atomically` is an alias of `commit` (the Effect.ts
name). It also returns an `Effect`, so run either one with `.block_on()` at the edge, await it
with `Runtime::run`, or bind it inside `effect!` like any other effect.

## Stm::fail: Transactional Errors

```rust,ignore
#[derive(Clone, Debug, PartialEq)]
struct InsufficientFunds;

fn withdraw(account: &TRef<u64>, amount: u64) -> Stm<u64, InsufficientFunds> {
  let acct = account.clone();
  account.read_stm().flat_map(move |balance| {
    if balance < amount {
      return Stm::fail(InsufficientFunds);
    }
    acct.write_stm(balance - amount).map(move |()| balance - amount)
  })
}
```

`Stm::fail(e)` aborts with `e`. The transaction is *not* restarted and none of its writes are
applied; `commit` surfaces `e` as the effect's error.

## Retry: wait for a condition

A transaction can wait until a condition holds instead of failing:

```rust,ignore
/// Restarts until the balance covers `amount`.
fn withdraw_when_available(account: &TRef<u64>, amount: u64) -> Stm<u64, ()> {
  let acct = account.clone();
  account.read_stm().flat_map(move |balance| {
    let acct = acct.clone();
    Stm::<(), ()>::check(balance >= amount)
      .flat_map(move |()| acct.write_stm(balance - amount).map(move |()| balance - amount))
  })
}
```

On retry, `commit` discards the attempt and parks the fiber on the wait lists of every `TRef` the
attempt read or wrote. The next commit that writes one of them wakes it, and the transaction runs
again. Nothing spins and no thread is blocked, so a retry also makes progress on a
single-threaded executor such as `TestExecutor`. A parked transaction is interruptible;
interrupting it removes its waker from every cell. A bare `Stm::retry()` that read no `TRef` waits
until it is interrupted. `TQueue::take` and `TSemaphore::acquire` work this way.

## or_else: try an alternative

`a.or_else(b)` runs `b` when `a` retries. The writes of the retrying branch are rolled back first,
so `b` sees the values from before `a` ran. It turns a would-block transaction into a non-blocking
try:

```rust,ignore
let try_now = withdraw_when_available(&account, 80).or_else(Stm::succeed(u64::MAX));
// Ok(u64::MAX) means "not now" instead of waiting.
```

## Composing Transactions

Transactions compose with `flat_map`. The result commits as one unit:

```rust,ignore
let audited = {
  let audit = audit.clone();
  withdraw(&account, 5).flat_map(move |left| audit.update_stm(|n| n + 1).map(move |()| left))
};
run(commit(audited))?;
```

If either part sees a conflict, the whole transaction restarts. If the withdrawal fails, the audit
counter is not incremented either.

Full example, including a waiter thread released by a deposit: [`crates/id_effect_fp/examples/085_stm_fail_retry.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_fp/examples/085_stm_fail_retry.rs) (`cargo run -p id_effect_fp --example 085_stm_fail_retry`).
