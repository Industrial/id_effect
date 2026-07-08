# run_test — The Test Harness

`run_test` is the test equivalent of `run_blocking`. Use it in every `#[test]` that runs an effect.

## Basic Usage

```rust
use id_effect::{run_test, succeed, Exit};

#[test]
fn simple_effect_succeeds() {
    let exit = run_test(succeed(42), ());
    assert_eq!(exit, Exit::Success(42));
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
    let exit = run_test(eff, ());

    // Assert specific failure
    assert!(matches!(exit, Exit::Failure(Cause::Fail(DivError::DivisionByZero))));
}

#[test]
fn effect_that_panics_is_a_defect() {
    let eff = effect!(|_r: &mut ()| {
        panic!("oops");
    });
    let exit = run_test(eff, ());

    assert!(matches!(exit, Exit::Failure(Cause::Die(_))));
}
```

`Exit::Success(a)` — the effect succeeded with value `a`
`Exit::Failure(Cause::Fail(e))` — the effect failed with typed error `e`
`Exit::Failure(Cause::Die(s))` — the effect panicked or encountered a defect
`Exit::Failure(Cause::Interrupt)` — the effect was cancelled

## run_test with an Environment

When your effect needs capabilities, build a test environment with `build_env` or manual `Env::insert`:

```rust
struct Database;

mock_capability!(MockDb, Database, Arc<dyn Db>, "db/mock", || {
    Arc::new(FakeDatabase::new()) as Arc<dyn Db>
});

#[test]
fn create_user_inserts_into_db() {
    let env = build_env([provide!(MockDb)]).expect("env");
    let fake_db = env.get::<Cap<Database>>().clone();

    let eff = create_user(NewUser { name: "Alice".into(), age: 30 });
    let exit = run_test(eff, env);

    assert!(matches!(exit, Exit::Success(_)));
    assert_eq!(fake_db.users().len(), 1);
}
```

`run_test(effect, env)` is the full signature. Pass `()` as the second argument when the effect requires no environment.

## Unwrapping success in tests

When you're confident an effect succeeds and just want the value, match on `Exit`:

```rust
#[test]
fn addition_works() {
    let exit = run_test(succeed(1 + 1), ());
    let Exit::Success(result) = exit else {
        panic!("expected success, got {exit:?}");
    };
    assert_eq!(result, 2);
}
```

Explicit matching keeps failure variants visible in the test — useful when a non-success `Exit` indicates a bug in the test setup.

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
    let exit = run_test(eff, ());
    // exit: Exit::Failure(Cause::Die("fiber leak detected: 1 fiber(s) not joined"))
}
```

Fix leaks by joining fibers or explicitly cancelling them before the effect completes.
