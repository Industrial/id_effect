# Laziness as a Superpower

`Effect<A, E, R>` is a *description* of a computation that does nothing until it is run. What do
you gain by waiting? Quite a bit, if your program benefits from composing and testing **before**
execution.

## Effect values vs driving an `async fn`

Rust futures are lazy too: calling an `async fn` returns a `Future`, and its body runs when the
future is polled. The difference is in **what your API returns**: a raw `Future` the caller must
await, or an `Effect` value it can store, wrap and run later with an explicit environment.

```rust,ignore
// Returns a Future; the HTTP work runs when it is awaited.
async fn fetch_user_async(id: u64) -> Result<User, HttpError> {
  http_get(&format!("https://api.example.com/users/{id}")).await
}

// Returns a description; the I/O runs when the effect is run with an `HttpClient`.
fn fetch_user(id: u64) -> Effect<User, HttpError, caps!(HttpClient)> {
  effect!({
    let http = ~HttpClient.clone();
    ~http.get_json(format!("https://api.example.com/users/{id}"))
  })
}
```

## Superpower #1: Compose First, Run Later

```rust,ignore
fn load_dashboard(user_id: u64) -> Effect<DashboardPage, AppError, caps!(Database, Cache)> {
  effect!({
    let user = ~fetch_user(user_id);
    let posts = ~fetch_posts(user.id);
    render(&user, &posts)
  })
}

// Nothing has run yet: `page` is a value.
let page = load_dashboard(42);

// Chain more work onto it; still nothing runs.
let logged = page.flat_map(log_view);

// Only now does any of it execute, with every capability wired once at the edge.
let exit = logged.provide_layer(app_layer()).block_on();
```

Everything before `block_on` assembles a pipeline that can be passed around, stored and
transformed.

## Superpower #2: Retry Without Rewriting

Because an effect is a description, you can wrap it with new behaviour without touching the
original:

```rust,ignore
use std::time::Duration;
use id_effect::{Effect, Schedule};

// Exponential back-off, at most 3 retries; `call_payment_api` is unchanged.
let resilient = Effect::retry(
  move || call_payment_api(order.clone()),
  Schedule::exponential(Duration::from_millis(100)).intersect(Schedule::recurs(3)),
);

// A timeout on top: `timeout` yields `None` if the deadline passes first.
let bounded: Effect<Option<Receipt>, PaymentError> = resilient.timeout(Duration::from_secs(5));
```

Effects are one-shot (running one consumes it), so `Effect::retry` takes a **factory** that builds
a fresh attempt each time.

## Superpower #3: Test Without Mocking the Universe

Because nothing runs until the environment is provided, a test provides a different one:

```rust
#[test]
fn user_not_found_returns_error() {
  let exit = fetch_user(99)
    .provide(HttpClient::stub_404("/users/99"))
    .block_on();
  assert!(matches!(exit, Exit::Failure(Cause::Fail(HttpError::NotFound))));
}
```

The same `fetch_user` that runs in production runs in the test, against another `HttpClient`
value. For code that sleeps or retries, `TestExecutor` runs it on a virtual clock
([Chapter 15](../part4/ch15-00-testing.md)).

## When Does It Actually Run?

An `Effect` executes only at an explicit edge:

```rust,ignore
// In a binary or another synchronous entry point
let exit = program.block_on();

// Inside async code (for example a Tokio task)
let exit = Runtime::global().run(program).await;

// In deterministic tests: fork the root fiber onto a TestExecutor and drive its virtual clock
let handle = program.fork_root(Arc::new(exec.clone()));
exec.run_until_stalled();
```

Everywhere else you are building, transforming or combining descriptions, so you always know where
side effects begin.

---

That's Chapter 1: **why** teams adopt effects (errors, dependencies, concurrency structure),
**what** an `Effect` is (a description run with an environment), what its type parameters mean, and
why keeping work in description form helps composition and testing.

Chapter 2 gets hands-on: first effects, `map`, `flat_map` and a small end-to-end program.
