# Laziness as a Superpower

`Effect<A, E, R>` is a *description* of a computation that does nothing until it is run. What do
you gain by waiting? Quite a bit, if your program benefits from composing and testing **before**
execution.

## Effect values vs driving an `async fn`

Rust futures are lazy too: calling an `async fn` returns a `Future`, and its body runs when the
future is polled. The difference is in **what your API returns**: a raw `Future` the caller must
await, or an `Effect` value it can store, wrap and run later with an explicit environment.

```rust
# use id_effect::{Effect, caps, effect, succeed};
# struct User; struct HttpError;
# async fn http_get(_url: &str) -> Result<User, HttpError> { Ok(User) }
# struct HttpClient;
# impl HttpClient { fn get_json(&self, _url: String) -> Effect<User, HttpError> { succeed(User) } }
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

```rust
# use id_effect::{Effect, HCons, HNil, Layer, caps, effect, hlist, succeed};
# struct User { id: u64 }
# struct Post; struct DashboardPage; struct AppError;
# struct Database; struct Cache;
# fn fetch_user(id: u64) -> Effect<User, AppError, caps!(Database)> { succeed(User { id }) }
# fn fetch_posts(_user: u64) -> Effect<Vec<Post>, AppError, caps!(Cache)> { succeed(vec![]) }
# fn render(_: &User, _: &[Post]) -> DashboardPage { DashboardPage }
# fn log_view(page: DashboardPage) -> Effect<DashboardPage, AppError, caps!(Database, Cache)> { succeed(page) }
# fn app_layer() -> Layer<HCons<Database, HCons<Cache, HNil>>, AppError> { Layer::succeed(hlist![Database, Cache]) }
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

```rust
use std::time::Duration;
use id_effect::{Effect, Schedule};
# #[derive(Clone)] struct Order;
# #[derive(Debug, PartialEq, Eq, Hash)] struct Receipt;
# #[derive(Debug, PartialEq, Eq, Hash)] struct PaymentError;
# fn call_payment_api(_: Order) -> Effect<Receipt, PaymentError> { id_effect::succeed(Receipt) }
# let order = Order;

// Exponential back-off, at most 3 retries; `call_payment_api` is unchanged.
let resilient = Effect::retry(
  move || call_payment_api(order.clone()),
  Schedule::exponential(Duration::from_millis(100)).intersect(Schedule::recurs(3)),
);

// A timeout on top: `timeout` yields `None` if the deadline passes first.
let bounded: Effect<Option<Receipt>, PaymentError> = resilient.timeout(Duration::from_secs(5));
# assert_eq!(bounded.block_on(), id_effect::Exit::succeed(Some(Receipt)));
```

Effects are one-shot (running one consumes it), so `Effect::retry` takes a **factory** that builds
a fresh attempt each time.

## Superpower #3: Test Without Mocking the Universe

Because nothing runs until the environment is provided, a test provides a different one:

```rust
# use id_effect::{Cause, Effect, Exit, caps, effect, fail, succeed};
# struct User;
# enum HttpError { NotFound }
# struct HttpClient { missing: &'static str }
# impl HttpClient {
#   fn stub_404(path: &'static str) -> Self { HttpClient { missing: path } }
#   fn get_json(&self, url: String) -> Effect<User, HttpError> {
#     if url.ends_with(self.missing) { fail(HttpError::NotFound) } else { succeed(User) }
#   }
# }
# fn fetch_user(id: u64) -> Effect<User, HttpError, caps!(HttpClient)> {
#   effect!({
#     let http = ~HttpClient.get_json(format!("https://api.example.com/users/{id}"));
#     ~http
#   })
# }
// In a #[cfg(test)] module, under #[test]:
fn user_not_found_returns_error() {
  let exit = fetch_user(99)
    .provide(HttpClient::stub_404("/users/99"))
    .block_on();
  assert!(matches!(exit, Exit::Failure(Cause::Fail(HttpError::NotFound))));
}
# user_not_found_returns_error();
```

The same `fetch_user` that runs in production runs in the test, against another `HttpClient`
value. For code that sleeps or retries, `TestExecutor` runs it on a virtual clock
([Chapter 15](../part4/ch15-00-testing.md)).

## When Does It Actually Run?

An `Effect` executes only at an explicit edge:

```rust
# use std::sync::Arc;
# use id_effect::{Effect, Runtime, TestExecutor, succeed};
# fn make_program() -> Effect<u32, String> { succeed(1) }
# let program = make_program();
// In a binary or another synchronous entry point
let exit = program.block_on();

# async fn in_async(program: Effect<u32, String>) {
// Inside async code (for example a Tokio task)
let exit = Runtime::global().run(program).await;
# }
# let program = make_program();
# let exec = TestExecutor::new();

// In deterministic tests: fork the root fiber onto a TestExecutor and drive its virtual clock
let handle = program.fork_root(Arc::new(exec.clone()));
exec.run_until_stalled();
# assert!(handle.is_done());
```

Everywhere else you are building, transforming or combining descriptions, so you always know where
side effects begin.

---

That's Chapter 1: **why** teams adopt effects (errors, dependencies, concurrency structure),
**what** an `Effect` is (a description run with an environment), what its type parameters mean, and
why keeping work in description form helps composition and testing.

Chapter 2 gets hands-on: first effects, `map`, `flat_map` and a small end-to-end program.
