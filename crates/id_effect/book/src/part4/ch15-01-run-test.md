# run_test — The Test Harness

`run_test` is the test equivalent of `run_blocking`. Use it in every `#[test]` that runs an effect.

## Basic Usage

```rust
use id_effect::{run_test, succeed};

#[test]
fn simple_effect_succeeds() {
    let result = run_test(succeed(42));
    assert_eq!(result, Exit::Success(42));
}
```

`run_test` returns an `Exit<A, E>` rather than a `Result<A, E>`. This lets you assert on the exact exit reason — success, typed failure, defect, or cancellation.

## Why Not run_blocking in Tests?

`run_blocking` is correct but missing test-specific guarantees:

| Feature | `run_blocking` | `run_test` |
|---------|---------------|------------|
| Runs the effect | ✓ | ✓ |
| Detects fiber leaks | ✗ | ✓ |
| Deterministic scheduling | ✗ | ✓ |
| Reports leaked resources | ✗ | ✓ |

Fiber leaks — effects that spawn children and don't join them — are silent in production but become test failures under `run_test`. This catches a class of resource leak bugs at unit-test time.

## Asserting on Exit

```rust
#[test]
fn division_by_zero_fails() {
    let eff = divide(10, 0);
    let exit = run_test(eff);

    // Assert specific failure
    assert!(matches!(exit, Exit::Failure(Cause::Fail(DivError::DivisionByZero))));
}

#[test]
fn effect_that_panics_is_a_defect() {
    let eff = effect!(|_r: &mut ()| {
        panic!("oops");
    });
    let exit = run_test(eff);

    assert!(matches!(exit, Exit::Failure(Cause::Die(_))));
}
```

`Exit::Success(a)` — the effect succeeded with value `a`
`Exit::Failure(Cause::Fail(e))` — the effect failed with typed error `e`
`Exit::Failure(Cause::Die(s))` — the effect panicked or encountered a defect
`Exit::Failure(Cause::Interrupt)` — the effect was cancelled

## run_test with an Environment

When your effect needs services, provide a test environment:

```rust
#[test]
fn create_user_inserts_into_db() {
    let fake_db = FakeDatabase::new();
    let env = ctx!(DbKey => Arc::new(fake_db.clone()));

    let eff = create_user(NewUser { name: "Alice".into(), age: 30 });
    let exit = run_test_with_env(eff, env);

    assert!(matches!(exit, Exit::Success(_)));
    assert_eq!(fake_db.users().len(), 1);
}
```

`run_test_with_env(effect, env)` is the full version. `run_test(effect)` is shorthand for `run_test_with_env(effect, ())` when the effect requires no environment.

## run_test_and_unwrap

When you're confident an effect succeeds and just want the value:

```rust
#[test]
fn addition_works() {
    let result: i32 = run_test_and_unwrap(succeed(1 + 1));
    assert_eq!(result, 2);
}
```

`run_test_and_unwrap` panics on any non-success `Exit`, with a descriptive message. Use it for happy-path tests where a failure is a bug in the test setup.

## Fiber Leak Detection

```rust
#[test]
fn this_test_will_fail_due_to_leak() {
    let eff = effect!(|_r: &mut ()| {
        // Spawns a fiber but never joins it
        run_fork(/* … */);
        ()
    });

    // run_test detects the leaked fiber and fails the test
    let exit = run_test(eff);
    // exit: Exit::Failure(Cause::Die("fiber leak detected: 1 fiber(s) not joined"))
}
```

Fix leaks by joining fibers or explicitly cancelling them before the effect completes.
