# The Three Type Parameters

Every `Effect` carries three type parameters: `Effect<A, E, R>`. These aren't arbitrary — they answer the three fundamental questions every computation must address:

- **A** — What do I produce when I succeed?
- **E** — What do I produce when I fail?
- **R** — What do I need in order to run?

Let's examine each one.

## A: The Answer

The `A` parameter is the success type — what you get back when everything goes right.

```rust
use id_effect::{Effect, succeed};

// This effect produces an i32 on success
let answer: Effect<i32, String, ()> = succeed(42);

// This effect produces a User on success
let user_effect: Effect<User, DbError, ()> = succeed(User::new("Alice"));
```

If you're familiar with `Result<T, E>`, think of `A` as the `T`. It's what you're hoping to get.

When you transform an effect with `.map()`, you're changing the `A`:

```rust
let numbers: Effect<i32, String, ()> = succeed(21);
let doubled: Effect<i32, String, ()> = numbers.map(|n| n * 2);
let stringified: Effect<String, String, ()> = doubled.map(|n| n.to_string());
```

Each `.map()` transforms the success value while preserving the error type and requirements.

## E: The Error

The `E` parameter is the failure type — what you get back when something goes wrong.

```rust
use id_effect::{Effect, fail};

// This effect always fails with a String error
let failure: Effect<i32, String, ()> = fail("something went wrong".to_string());

// This effect can fail with a DbError
let user: Effect<User, DbError, ()> = fetch_user_from_db(42);
```

Again, if you know `Result<T, E>`, think of `E` as the `E`. It's what you're worried might happen.

You can transform error types with `.map_error()`:

```rust
let db_effect: Effect<User, DbError, ()> = fetch_user(42);

// Convert DbError to a more general AppError
let app_effect: Effect<User, AppError, ()> = db_effect.map_error(|e| AppError::Database(e));
```

Unlike traditional error handling where you sprinkle `.map_err()` everywhere, with effects you typically handle error transformation at specific boundaries — when composing larger effects from smaller ones, or when exposing an API.

## R: The Requirements

Here is where effects get interesting. The `R` parameter represents the **environment** — the dependencies this effect needs in order to run.

When an effect needs services, express `R` with [`caps!`](../part2/ch04-00-r-parameter.md) and capability **keys** (Chapter 5 names keys fully; use `Database`, not a bare `Database` type):

```rust
use id_effect::{Effect, caps, effect, provide, require, run_with, succeed};

// Self-contained — R is ()
let standalone: Effect<i32, String, ()> = succeed(42);

// Needs Database at the edge
fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)> {
    effect!(|r| {
        let db = ~Database;
        Ok(db.fetch_user(id))
    })
}

// Needs two keys
fn get_user_logged(id: u64) -> Effect<User, DbError, caps!(Database, EffectLogger)> {
    effect!(|r| {
        let db = ~Database;
        let log = ~EffectLogger;
        let user = db.fetch_user(id)?;
        log.info(&format!("fetched {}", user.id));
        Ok(user)
    })
}
```

**You cannot run an effect until its capabilities exist.** Satisfy them at the program edge with [`run_with`](../part2/ch04-02-providing.md), not inside library code:

```rust
run_with([provide!(DatabaseLive)], get_user(42))?;
```

There is no `.provide()` on effects. [`run_with`](../part2/ch04-02-providing.md) builds an [`Env`](../part2/ch05-03-context-hlists.md) and executes the program.

## Why R matters

The `R` parameter is why id_effect offers compile-time dependency injection.

```rust
fn process_order(order: Order) -> Effect<
    Receipt,
    OrderError,
    caps!(Database, PaymentGateway, EmailService, EffectLogger),
>
```

Just from the type you know success, error, and **which capability services** must be wired before `run_with`.

## R flows through composition

```rust
fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)> { ... }
fn send_email(to: &str, body: &str) -> Effect<(), EmailError, caps!(EmailService)> { ... }

fn notify_user(id: u64) -> Effect<(), AppError, caps!(Database, EmailService)> {
    effect!(|r| {
        let user = ~ get_user(id).map_error(AppError::Db);
        ~ send_email(&user.email, "Hello!").map_error(AppError::Email);
        ()
    })
}
```

## The unit environment: ()

When `R = ()`, the effect is self-contained:

```rust
let standalone: Effect<i32, String, ()> = succeed(42);
let result = run_blocking(standalone, ());
```

Effects with dependencies keep `caps!(…)` on the type until the edge:

```rust
fn main() -> Result<(), AppError> {
    run_with(
        [
            provide!(DatabaseLive),
            provide!(CacheLive),
            provide!(LoggerLive),
            provide!(ConfigLive),
        ],
        business_logic(),
    )
}
```


## Reading Effect Signatures

Let's practice reading some signatures:

```rust
// Produces String, never fails, needs nothing
Effect<String, Never, ()>

// Produces i32, can fail with ParseError, needs nothing
Effect<i32, ParseError, ()>

// Produces User, can fail with DbError, needs Database
Effect<User, DbError, caps!(Database)>

// Produces (), can fail with AppError, needs four capability services
Effect<(), AppError, caps!(Database, Cache, EffectLogger)>
```

With practice, you'll read these as fluently as you read `Result<T, E>`. The extra `R` parameter becomes second nature.

## What's Next

We've seen that effects are descriptions, not actions. We've seen that `Effect<A, E, R>` encodes success type, error type, and requirements.

But we haven't answered the obvious question: why does this matter? Why is it better to describe computations than to just do them?

The answer is laziness. And laziness, it turns out, is a superpower.
