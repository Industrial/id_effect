# What Even Is an Effect?

**An Effect is a description of a computation, not the computation itself.**

The rest of the API (`map`, `flat_map`, the environment type, the run edges) is there to work with
that description in a type-safe way.

## The Recipe Analogy

A recipe is not a cake. You can hold a recipe, photocopy it, change it (less sugar, more cocoa),
combine it with a frosting recipe and hand it to a friend, and no cake appears. The cake appears
only when someone *executes* the recipe.

An `Effect` is a recipe for a computation. `succeed(42)` does not "succeed" at anything: it is a
recipe that says "when run, produce 42".

```rust
use id_effect::{Effect, succeed};

// Nothing is computed here: `recipe` is just a value.
let recipe: Effect<i32, String> = succeed(42);
```

The computation happens only when you run it at the edge of the program:

```rust
use id_effect::Exit;

// NOW something happens.
let exit: Exit<i32, String> = recipe.block_on();
assert_eq!(exit, Exit::succeed(42));
```

Running an effect gives you an [`Exit`](../part3/ch08-02-exit.md): `Exit::Success(a)` or
`Exit::Failure(cause)`. `exit.into_result()` turns it into a `Result<A, Cause<E>>`.

## Building Up Descriptions

Because an effect is a value, you can transform it without running it:

```rust
use id_effect::{Effect, Exit, succeed};

let recipe: Effect<i32, String> = succeed(42);

// "When run, produce 42, then double it." Still nothing has happened.
let doubled: Effect<i32, String> = recipe.map(|x| x * 2);

assert_eq!(doubled.block_on(), Exit::succeed(84));
```

`.map()` did not execute anything. It took one recipe and produced a new one with an extra step.

## A More Realistic Example

```rust
use id_effect::{Effect, effect};

// This function fetches nothing: it returns a DESCRIPTION of how to fetch a user.
fn fetch_user(id: u64) -> Effect<User, DbError> {
  effect!({
    let conn = ~connect_to_db();
    let user = ~query_user(&conn, id);
    user
  })
}

// Calling the function opens no connection.
let description = fetch_user(42);

// Only running it does the I/O.
let user = description.block_on().into_result()?;
```

The `effect!` block looks imperative, but it builds a description. `~expr` means "run this
sub-effect and give me its value", in order, when the outer effect runs.

## The Key Insight: Separation of Concerns

Separating description from execution gives the challenges of the previous section a consistent
home in the type system.

**Error handling** becomes part of the description:

```rust
use std::time::Duration;
use id_effect::{Effect, Schedule};

// Effects are one-shot values, so `retry` takes a factory that builds a fresh attempt.
let resilient = Effect::retry(
  || risky_operation(),
  Schedule::exponential(Duration::from_millis(100)).intersect(Schedule::recurs(3)),
);
```

You are not adding retry logic to running code; you are building a new recipe that says "when
run, retry up to 3 times with exponential backoff".

**Dependencies** become part of the type signature:

```rust
fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)>
```

`caps!(Database)` says "this recipe needs a `Database` capability to run". The compiler enforces
it: `block_on` only exists for effects whose requirements have all been provided, so a missing
capability is a compile error, not a runtime surprise.

**Structured concurrency** follows because forking an effect returns a handle to a child fiber
with clear ownership and interruption semantics, instead of a fire-and-forget task.

## What's in an Effect?

An `Effect<A, E = Never, R = HNil>` carries three pieces of information in its type:

- `A`: the **Answer**, what you get if it succeeds;
- `E`: the **Error**, what you get if it fails (default `Never`: it cannot fail);
- `R`: the **Requirements**, the capabilities needed to run it (default `HNil`: nothing).

```rust
// The signature tells the whole story:
fn process_payment(amount: Money)
  -> Effect<Receipt, PaymentError, caps!(PaymentGateway, AuditLog)>
// - produces a Receipt on success
// - can fail with PaymentError
// - needs PaymentGateway and AuditLog to run
```

An `Effect` is `Send` and one-shot: running it consumes it. To run "the same" work twice, call the
function that builds it twice.

## Style: imperative async vs effect descriptions

`async fn` code is written as a sequence of `.await`s. That is clear and idiomatic Rust, and the
effect runtime is built on the same `Future` machinery.

Effect code makes many domain functions return `Effect<…>`: a value that describes work and runs
only when you pass it to an edge with its environment. Use effects where you want the environment
and the error structure in the type, shared policies (retry, timeout, spans) and test
substitution at the boundary; use plain `async fn` where a small linear function is enough.

Let's look at the three type parameters in detail.
