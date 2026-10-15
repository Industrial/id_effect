# Racing and timeouts: `race`, `race_all`, `race_first`, `timeout`, `timeout_fail`

0.7 adds racing on fibers (Effect.ts `Effect.race` / `raceAll` / `raceFirst` / `timeout` /
`timeoutFail`). Every racer runs as a child fiber of the racing fiber.

```rust
// 0.7
slow.race(fast)                                  // Effect<A, E, R>: first success wins
race_all([a, b, c])                              // first success among many
a.race_first(b)                                  // first completion wins, success or failure
call.timeout(Duration::from_secs(2))             // Effect<Option<A>, E, R>: None on timeout
call.timeout_fail(Duration::from_secs(2), || AppError::Timeout) // fails with your error
```

- The losers are interrupted and awaited: their finalizers have run before the result is
  returned. A loser's own outcome after the decision is dropped.
- `race` / `race_all`: a failure does not end the race. When every racer fails, the cause is
  every failure combined with `Cause::Both` in input order (`a.race(b)` fails with
  `Both(cause_a, cause_b)`). Unlike Effect.ts `raceAll` (last failure only), `race_all` keeps
  them all, so `race_all([a, b])` and `a.race(b)` agree. `race_all` of no effects dies.
- `timeout(d)` returns `Option<A>` rather than failing with a `TimeoutException`; use
  `timeout_fail` for a typed error. The deadline is the fiber's executor clock, so it is
  virtual time on `TestExecutor` (`advance(10s)` fires a `timeout(10s)` at once).
- An uninterruptible (masked) effect delays its timeout until it completes; the result is still
  `None`.
