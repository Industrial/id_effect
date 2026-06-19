---
name: id_effect-testing
description: >-
  Expert in testing id_effect code: run_test harness, Exit assertions, TestClock,
  mock_capability!, build_env/provide! test providers, and property testing. Never
  mock id_effect internals with module mocks — use capability DI at the test edge.
---

# id_effect Testing

**Part IV ch15**. Critical rule: **test through DI, not module mocks**.

**Prerequisites**: `id_effect-fundamentals`, `id_effect-capabilities`.

## run_test — always in unit tests

```rust
use id_effect::{run_test, succeed, Exit};

#[test]
fn succeeds() {
    let exit = run_test(succeed(42), ());
    assert_eq!(exit, Exit::Success(42));
}
```

| Feature | `run_blocking` | `run_test` |
|---------|---------------|------------|
| Runs effect | ✓ | ✓ |
| Fiber leak detection | ✗ | ✓ |
| Deterministic scheduling | ✗ | ✓ |
| Full `Exit` taxonomy | ✗ | ✓ |

## Asserting failures

```rust
assert!(matches!(exit, Exit::Failure(Cause::Fail(DivError::DivisionByZero))));
assert!(matches!(exit, Exit::Failure(Cause::Die(_))));
```

Prefer explicit `Exit` matching over `.unwrap()` — non-success often means broken test setup.

## Mock capabilities

```rust
mock_capability!(MockDb, DatabaseKey, Arc<dyn Db>, "db/mock", || {
    Arc::new(FakeDatabase::new()) as Arc<dyn Db>
});

#[test]
fn create_user() {
    let env = build_env([provide!(MockDb)]).expect("env");
    let exit = run_test(create_user(user), env);
    assert!(matches!(exit, Exit::Success(_)));
}
```

Swap **`provide!(Mock…)`** at the edge; domain code using `caps!(DatabaseKey)` stays unchanged.

Custom fixtures: `env.insert::<UserRepoKey>(Arc::new(mock))`.

## TestClock

For `Schedule`, retry, and timeout tests — inject test clock via capabilities; advance time deterministically (ch15-02).

## Never do this

```rust
// WRONG — bypasses DI, leaks between tests, breaks types
#[cfg(test)]
mod mock { /* replace entire module */ }
```

Use **`mock_capability!`**, **`provide!(MockLive)`**, or **`env.insert`** instead.

## Property testing

Use proptest/quickcheck on pure functions and schema round-trips at boundaries (ch15-04).

## Verify

```bash
cargo test --workspace
cargo test -p id_effect --test ui_compile_fail
```

## Next

- Review checklist: [id_effect-review](../id_effect-review/SKILL.md)

## Property testing helpers (Part V ch24)

Core helpers (always available):

```rust
use id_effect::{run_effect, exit_success_value, assert_exit_success, succeed};

let exit = run_effect(succeed(42), ());
assert_exit_success(&exit, &42);
```

Enable `id_effect` feature `proptest` for `success_value`, `prop_assert_exit_success`, and `arb_exit_*` strategies.

## Law tests

```rust
use id_effect::law_test;

law_test! {
  monad option_i32 {
    pure = option::pure,
    flat_map = option::flat_map,
    fa = Some(3),
    a = 7,
    f = my_inc_fn,   // fn items, not closures
    g = my_double_fn,
  }
}
```

## Golden snapshots

```rust
use id_effect::{assert_golden_effect, GoldenBuilder, snapshot_effect_map_flat_map};

assert_golden_effect(snapshot_effect_map_flat_map(), ());
GoldenBuilder::new("name", "expected").assert_observed("observed");
```

## Pretty failures

```rust
use id_effect::{pretty_cause, pretty_exit};
println!("{}", pretty_exit(&exit));
```
