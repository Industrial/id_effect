# R Revisited — More Than Just a Type Parameter

You've seen `R` in function signatures:

```rust
fn get_user(id: u64) -> Effect<User, DbError, caps!(DatabaseKey)>
```

It looks like "this needs `DatabaseKey`." But what does that mean precisely?

## R as a contract

`R` is a *promise to the compiler*. When you write:

```rust
fn get_user(id: u64) -> Effect<User, DbError, caps!(DatabaseKey)> {
    effect!(|r| {
        let db = ~DatabaseKey;
        Ok(db.fetch_user(id))
    })
}
```

You are declaring: "To run this effect, you must supply `DatabaseKey` in the environment." The compiler holds you to that promise. You cannot call `run_with` without a provider for `DatabaseKey`.

```rust
// Missing DatabaseLive in the provider list → runtime CapabilityError at run_with
// run_with([], get_user(1))?;

// Correct — graph builds DatabaseKey before the effect runs
run_with([provide!(DatabaseLive)], get_user(1))?;
```

The contract is not a comment. It is enforced by `caps!(…)` on the effect type and by [`run_with`](../../src/capability/run.rs) at the edge.

## R flows through composition

When you combine effects with `effect!`, their capability requirements merge:

```rust
fn get_user(id: u64) -> Effect<User, DbError, caps!(DatabaseKey)> { ... }
fn get_posts(user_id: u64) -> Effect<Vec<Post>, DbError, caps!(DatabaseKey)> { ... }

// Combined: still caps!(DatabaseKey) — both needed the same key
fn get_user_with_posts(id: u64) -> Effect<(User, Vec<Post>), DbError, caps!(DatabaseKey)> {
    effect!(|r| {
        let user = ~ get_user(id);
        let posts = ~ get_posts(user.id);
        (user, posts)
    })
}
```

When effects need *different* keys:

```rust
fn log(msg: &str) -> Effect<(), LogError, caps!(LoggerKey)> { ... }
fn get_user(id: u64) -> Effect<User, DbError, caps!(DatabaseKey)> { ... }

// Combined: caps!(DatabaseKey, LoggerKey) — needs BOTH
fn get_user_logged(id: u64) -> Effect<User, AppError, caps!(DatabaseKey, LoggerKey)> {
    effect!(|r| {
        ~ log(&format!("Fetching user {id}")).map_error(AppError::Log);
        let user = ~ get_user(id).map_error(AppError::Db);
        user
    })
}
```

The composed effect's `caps!(…)` list is the union of what each step needs. You wire every key once at `main` or in tests:

```rust
run_with(
    [provide!(DatabaseLive), provide!(LoggerLive)],
    get_user_logged(42),
)?;
```

## Multiple requirements

As functions grow, they naturally accumulate keys:

```rust
fn process_order(order: Order) -> Effect<
    Receipt,
    AppError,
    caps!(DatabaseKey, PaymentGatewayKey, EmailServiceKey, LoggerKey),
> {
    effect!(|r| {
        ~ log("Processing order").map_error(AppError::Log);
        let user = ~ get_user(order.user_id).map_error(AppError::Db);
        let payment = ~ charge(order.total).map_error(AppError::Payment);
        ~ send_confirmation(&user.email).map_error(AppError::Email);
        Receipt::new(payment)
    })
}
```

Just from the type signature you know this function touches four capability keys. No need to read the implementation.

## Why R instead of parameters?

Traditional Rust would thread dependencies as function parameters:

```rust
fn process_order(
    order: Order,
    db: &Database,
    pay: &PaymentGateway,
    email: &EmailService,
    log: &Logger,
) -> Result<Receipt, AppError> { ... }
```

That works, but it forces every layer of your call stack to accept and forward dependencies it may not directly use. The `R` parameter encodes the same information in the *return type* — and [`caps!(…)`](../../src/capability/set.rs) names each dependency so two services of the same Rust type remain distinct.

## Foreshadowing

You may be wondering: how does the runtime store `DatabaseKey` and `LoggerKey` in one place?

[`Env`](../../src/capability/env.rs) is an order-independent map keyed by capability identity — not a positional tuple. Chapter 5 shows how `#[capability]` generates each `*Key` type. For now: **`R = caps!(…)` is the compile-time list; `Env` is the runtime container.**
