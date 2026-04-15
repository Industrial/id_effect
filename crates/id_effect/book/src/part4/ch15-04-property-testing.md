# Property Testing — Invariants over Inputs

Unit tests check specific cases. Property tests check invariants: statements that must be true for *any* valid input. Effect programs are excellent targets for property testing because their inputs and outputs are well-typed, their schemas define exactly what's valid, and the layer system makes it easy to run thousands of executions cheaply.

## Setup

id_effect works with both [`proptest`](https://github.com/proptest-rs/proptest) and [`quickcheck`](https://github.com/BurntSushi/quickcheck). The examples below use `proptest`.

```toml
[dev-dependencies]
proptest = "1"
```

## Testing Pure Effects

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn addition_is_commutative(a: i64, b: i64) {
        let eff_ab = add(a, b);
        let eff_ba = add(b, a);

        let r_ab = run_test_and_unwrap(eff_ab);
        let r_ba = run_test_and_unwrap(eff_ba);

        prop_assert_eq!(r_ab, r_ba);
    }
}
```

`proptest!` generates hundreds of `(a, b)` pairs. Each iteration calls `run_test_and_unwrap`, which is cheap for pure effects.

## Testing Schema Round-Trips

Schemas have a round-trip property: if you serialise a valid value and re-parse it, you get the same value back.

```rust
proptest! {
    #[test]
    fn user_schema_round_trips(
        name in "[a-zA-Z]{1,50}",
        age in 0i64..=120,
    ) {
        let original = User {
            name: name.clone(),
            age,
        };

        // Serialise to Unknown
        let raw = User::schema().encode(&original);

        // Re-parse
        let parsed = User::schema().run(raw);

        prop_assert!(parsed.is_ok());
        prop_assert_eq!(parsed.unwrap(), original);
    }
}
```

Round-trip tests catch asymmetries between your serialiser and parser that unit tests often miss.

## Testing Error Invariants

Property tests are excellent for verifying that your error handling is consistent:

```rust
proptest! {
    #[test]
    fn withdraw_never_goes_negative(
        balance in 0u64..=1_000_000,
        amount  in 0u64..=1_000_000,
    ) {
        let account = TRef::new(balance);
        let exit = run_test_and_unwrap(commit(withdraw(&account, amount)));

        if amount <= balance {
            // Should succeed and balance should be reduced
            assert!(matches!(exit, Exit::Success(_)));
            let new_balance = atomically(account.read_stm());
            assert_eq!(new_balance, balance - amount);
        } else {
            // Should fail — balance must not go negative
            assert!(matches!(exit, Exit::Failure(Cause::Fail(InsufficientFunds))));
            let new_balance = atomically(account.read_stm());
            assert_eq!(new_balance, balance);  // unchanged
        }
    }
}
```

## Generating Arbitrary Service Environments

For integration-style property tests, generate random state in the fake service:

```rust
proptest! {
    #[test]
    fn get_user_returns_what_was_saved(user in arbitrary_user()) {
        let db = Arc::new(InMemoryDb::new());
        let env = ctx!(DbKey => db.clone() as Arc<dyn Db>);

        // Save
        run_test_with_env(
            save_user(user.clone()),
            env.clone(),
        );

        // Retrieve
        let exit = run_test_with_env(get_user(user.id), env);
        let retrieved = exit.unwrap_success();

        prop_assert_eq!(retrieved, user);
    }
}
```

Define `arbitrary_user()` as a `proptest` `Strategy`:

```rust
fn arbitrary_user() -> impl Strategy<Value = User> {
    (
        "[a-zA-Z ]{1,50}",
        0i64..=120,
        any::<u64>().prop_map(UserId::new),
    ).prop_map(|(name, age, id)| User { id, name, age })
}
```

## Schema-Driven Generation

When a type has `HasSchema`, you can derive a generator that always produces valid inputs:

```rust
// generate_valid::<User>() produces Users that would pass User::schema()
let strategy = generate_valid::<User>();

proptest! {
    #[test]
    fn valid_users_are_always_accepted(user in generate_valid::<User>()) {
        let raw = User::schema().encode(&user);
        prop_assert!(User::schema().run(raw).is_ok());
    }
}
```

This ensures the generator and schema stay in sync: if you tighten a `refine` constraint, `generate_valid` starts producing inputs that satisfy the new constraint.

## Shrinking

`proptest` automatically shrinks failing inputs to the smallest example that still fails. Since `run_test` is fast (no I/O, no real timers), shrinking runs quickly even with hundreds of iterations.

When a property fails, you'll see the minimal failing case:

```
Test failed. Minimal failing input:
  name = ""
  age = -1
Reason: must not be empty (path: name)
```

This is far more actionable than a raw failure trace from a specific hand-chosen test case.
