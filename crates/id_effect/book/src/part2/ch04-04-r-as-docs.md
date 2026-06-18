# R as Documentation — Self-Describing Functions

The `R` parameter is often described as "the environment type." That's true, but it undersells the practical benefit. `R` is *living documentation* that the compiler enforces.

## The Signature Tells the Story

Consider two versions of the same function:

```rust
// Version A: traditional async
async fn process_order(order: Order) -> Result<Receipt, Error> {
    // What does this use? Read the body to find out.
    // Database? PaymentGateway? Email? Metrics?
    // You'll have to trace through 200 lines to know.
}

// Version B: effect-based
fn process_order(order: Order) -> Effect<
    Receipt,
    OrderError,
    caps!(DatabaseKey, PaymentGatewayKey, EmailServiceKey, LoggerKey),
> {
    // What does this use? Look at the signature.
    // Database ✓, PaymentGateway ✓, EmailService ✓, Logger ✓
    // Done.
}
```

Version B's type is self-describing. You don't need to read the implementation to understand its dependency surface.

## Code Review Benefits

In a pull request, `R` changes are visible in the diff. If someone adds a call to `send_metrics()` inside `process_order` and `MetricsClientKey` wasn't previously in `R`, the function signature must change:

```diff
- fn process_order(order: Order) -> Effect<Receipt, OrderError, caps!(DatabaseKey, PaymentGatewayKey, EmailServiceKey, LoggerKey)>
+ fn process_order(order: Order) -> Effect<Receipt, OrderError, caps!(DatabaseKey, PaymentGatewayKey, EmailServiceKey, LoggerKey, MetricsClientKey)>
```

This diff is in the function signature — impossible to miss. With traditional parameters or singletons, new dependencies can silently appear in implementation bodies.

## Refactoring Safety

When you refactor and remove a dependency, the compiler finds all the places that were providing the now-unnecessary value. The `R` type shrinks, and provider lists that still register removed keys become visible during review.

```rust
// After removing LoggerKey from process_order's caps! list:

// Callers must drop LoggerLive from run_with when nothing in the graph needs it
run_with(
    [provide!(DatabaseLive)], // LoggerLive no longer required by this effect
    process_order(order),
)?;
```

The compiler guides you to clean up wiring. Traditional code leaves stale dependencies silently lingering.

## Testing Clarity

When writing a test, `R` tells you exactly what you need to mock:

```rust
#[test]
fn test_process_order() {
    // R = caps!(DatabaseKey, PaymentGatewayKey, EmailServiceKey, LoggerKey)
    // So the test needs these four keys — no more, no less
    let mut env = Env::new();
    env.insert::<DatabaseKey>(mock_db());
    env.insert::<PaymentGatewayKey>(mock_payment());
    env.insert::<EmailServiceKey>(mock_email());
    env.insert::<LoggerKey>(test_logger());

    let result = run_test(process_order(test_order()), env);
    assert!(result.is_ok());
}
```

There's no "I wonder if this also touches the metrics service" uncertainty. The type says it doesn't. If you're missing a mock, the code won't compile or `require!` fails at runtime.

## R is Not Magic

It's important to understand that `R` is just a type parameter. The "compile-time DI" property comes from:

1. Functions declaring what they need in `R` (usually via `caps!`)
2. Capability keys identifying services in [`Env`](../../src/capability/env.rs)
3. Composition automatically merging requirements
4. Wiring centralized at `run_with` / `build_env` boundaries

There's no reflection, no registration, no framework. Just types.

The next chapter shows how **capability keys** and **`Env`** make this scale beyond simple capability lists — handling large, complex dependency graphs without positional ambiguity.
