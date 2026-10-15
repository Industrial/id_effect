# Migrating from `async fn` to effects

> **Upgrading id_effect 0.6 → 0.7:** the runtime capability DI (`Env`, `ProviderSpec`,
> `run_with`, `provide!`, `require!`), `run_blocking` / `run_test` and `TestClock` are gone; `R`
> is a typed HList, dependencies come from `Layer`s, and effects run with `block_on`. See the
> [0.7 migration guide](https://github.com/Industrial/id_effect/blob/main/docs/MIGRATION-0.7.md).
> For 0.5 → 0.6 see the
> [0.6 migration guide](https://github.com/Industrial/id_effect/blob/main/docs/MIGRATION-0.6.md).

This appendix is a practical guide for converting existing async Rust code to id_effect. It covers
common patterns and their id_effect equivalents. The snippets are sketches: `Database`, `Mailer`,
`User` and friends stand for your own types.

## The Mental Model Shift

In typical async Rust, a function returns a `Future`; when that future is awaited, the work runs:

```rust,ignore
async fn get_user(id: u64, db: &DbClient) -> Result<User, DbError> {
    db.query_one("SELECT * FROM users WHERE id = $1", &[&id]).await
}
```

In id_effect, domain functions return an **`Effect`**: a description that runs later, once its
capabilities have been provided:

```rust,ignore
use id_effect::{Effect, caps, effect};

fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)> {
    effect!({
        let db = Arc::clone(~Database);
        ~db.get_user(id)
    })
}
```

The database client is no longer a function parameter. It is declared in `R` with
`caps!(Database)` and read with `~Database` inside `effect!`. The business logic is the same; what
changes is where the dependency comes from: `.provide(db)` or a `Layer` at the edge. Forgetting to
provide it is a compile error.

## Pattern 1: async fn → fn returning Effect

**Before**

```rust,ignore
pub async fn process_order(
    order_id: OrderId,
    db: &DbClient,
    mailer: &MailClient,
) -> Result<Receipt, AppError> {
    let order = db.get_order(order_id).await?;
    let receipt = db.complete_order(order).await?;
    mailer.send_receipt(&receipt).await?;
    Ok(receipt)
}
```

**After**

```rust,ignore
pub fn process_order(order_id: OrderId) -> Effect<Receipt, AppError, caps!(Database, Mailer)> {
    effect!({
        let db = Arc::clone(~Database);
        let mailer = Arc::clone(~Mailer);
        let order = ~db.get_order(order_id);
        let receipt = ~db.complete_order(order);
        ~mailer.send_receipt(receipt.clone());
        receipt
    })
}

// At the edge:
let exit = process_order(id)
    .provide_layer(DatabaseLive::layer().merge(MailerLive::layer()))
    .block_on();
```

**Migration steps:**

1. Remove dependency parameters (`db`, `mailer`).
2. Declare service types (`struct Database { .. }` or a newtype over `Arc<dyn DbClient>`).
3. Return `Effect<_, _, caps!(Database, Mailer)>`.
4. Replace `async move { … }` with `effect!({ … })`; `~Cap` reads a capability.
5. Replace `.await?` on effects with a `~` prefix (`.await` on plain futures and `?` on `Result`s
   still work in the body); `~(a, b)` runs two effects in parallel.
6. Provide the capabilities at the edge: `.provide(v)`, `.provide_env(hlist![..])` or
   `.provide_layer(layer)`, then `.block_on()` (or `Runtime::run(eff).await` in async code).

## Pattern 2: Wrapping Third-Party Async

Third-party libraries return `Future`s, not `Effect`s. Use `from_async` to wrap them:

**Before**

```rust,ignore
async fn fetch_price(symbol: &str) -> Result<f64, reqwest::Error> {
    reqwest::get(format!("https://api.example.com/price/{symbol}"))
        .await?
        .json::<PriceResponse>()
        .await
        .map(|r| r.price)
}
```

**After**

```rust,ignore
use id_effect::{Effect, from_async};

fn fetch_price(symbol: String) -> Effect<f64, reqwest::Error> {
    from_async(move || async move {
        reqwest::get(format!("https://api.example.com/price/{symbol}"))
            .await?
            .json::<PriceResponse>()
            .await
            .map(|r| r.price)
    })
}
```

The closure takes no arguments and its future must be `Send + 'static`. The future still uses
`.await` internally; only the outer signature changes.

## Pattern 3: Error Types

**Before**: a single monolithic error enum threaded through every function.

```rust,ignore
#[derive(Debug)]
enum AppError {
    Db(DbError),
    Mail(MailError),
    NotFound(String),
}
```

**After**: each function fails with its own error, and callers widen it. `~expr` in `effect!`
converts the sub-effect's error with `From`; outside the macro use `map_err`.

```rust,ignore
fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)> { /* … */ }

impl From<DbError> for AppError {
    fn from(e: DbError) -> Self { AppError::Db(e) }
}

fn handler(id: u64) -> Effect<Response, AppError, caps!(Database)> {
    effect!({
        let user = ~get_user(id); // DbError -> AppError
        Response::ok(user)
    })
}
```

`err!(A | B)` spells the anonymous union `Or<A, B>` when you don't want a named enum.

## Pattern 4: Shared State

**Before**: `Arc<Mutex<T>>` passed through function calls.

```rust,ignore
async fn handler(state: Arc<Mutex<AppState>>) -> Response {
    let mut s = state.lock().unwrap();
    s.request_count += 1;
    // …
}
```

**After**: a `Ref` (or any shared value) as a capability.

```rust,ignore
use id_effect::{Effect, Ref, caps, effect};

struct RequestCount(Ref<u64>);

fn handler() -> Effect<Response, AppError, caps!(RequestCount)> {
    effect!({
        let counter = Arc::clone(~RequestCount);
        let count = ~counter.0.update_and_get(|n| n + 1);
        Response::ok(count)
    })
}
```

For transactional state across several cells, use `TRef` + STM from `id_effect_fp`.

## Pattern 5: Resource Cleanup

**Before**: manual cleanup, or relying on `Drop` impls that cannot be async.

```rust,ignore
async fn with_connection<T>(pool: &Pool, f: impl AsyncFnOnce(&Conn) -> Result<T, DbError>)
    -> Result<T, DbError>
{
    let conn = pool.get().await?;
    f(&conn).await
}
```

**After**: `acquire_use_release` (or a `Scope`) runs the release on success, failure and
interruption.

```rust,ignore
use id_effect::{Effect, ExitKind, acquire_use_release};

fn with_connection<A>(
    f: impl FnOnce(Conn) -> Effect<A, DbError> + Send + 'static,
) -> Effect<A, DbError> {
    acquire_use_release(
        open_connection(),                      // Effect<Conn, DbError>
        f,
        |conn, _exit: ExitKind| close(conn),    // Effect<(), Never>
    )
}
```

`Effect::scoped(|scope| ..)` with `scope.add_finalizer(..)` covers several resources at once, and
`Pool::new(capacity, || acquire)` with `pool.get(&scope)` reuses them.

## Pattern 6: Tests

**Before**: a mocking framework, or test-only constructor parameters.

**After**: provide a test value of the same capability type and run with `block_on`; for time,
run on a `TestExecutor` and move its virtual clock.

```rust,ignore
#[test]
fn reads_the_user() {
    let exit = get_user(1).provide(Database::in_memory()).block_on();
    assert_eq!(exit, Exit::succeed(User::fixture()));
}
```

## HTTP boundaries: raw `reqwest` → workspace crates

After effects replace bare `async fn`, move HTTP edges toward **typed capabilities**:

1. **`id_effect_platform`** — portable HTTP, file system and subprocess capabilities (see
   [Platform I/O](./part2/ch07-06-platform-services.md)).
2. **`id_effect_platform::http::reqwest`** — the reqwest backend (see
   [HTTP via reqwest](./part2/ch07-07-reqwest-http.md)).

Host either style under **Axum** with **`id_effect_axum`** ([Axum host](./part2/ch07-08-axum-host.md)).

## Migration Strategy

Migrate gradually, one module at a time:

1. Start with leaf functions (no effect dependencies yet).
2. Move up the call graph.
3. Push `block_on` (or `Runtime::run(..).await`) to `main` or the request handler.
4. Convert tests last: provide test values or test layers instead of mocks.

You can mix async functions and effects during the transition: wrap async code with
`from_async`, and call effects from async code with `Runtime::global().run(eff).await`.

See Part II (chapters 4–7) for the full capability narrative.
