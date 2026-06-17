# Migrating from `async fn` to effects

This appendix is a practical guide for converting existing async Rust code to id_effect. It covers common patterns and their id_effect equivalents, with migration steps for each.

> **Capability DI v2:** If you are migrating from id_effect v1's tag/HList DI (`service_key!`, `ctx!`, `Layer`/`Stack`), skip to [Migrating v1 DI to capability DI v2](#migrating-v1-di-to-capability-di-v2) first.

## The Mental Model Shift

In typical async Rust, a function returns a `Future`; when that future is awaited, the work runs:

```rust
async fn get_user(id: u64, db: &DbClient) -> Result<User, DbError> {
    db.query_one("SELECT * FROM users WHERE id = $1", &[&id]).await
}
```

In id_effect, domain functions return an **`Effect`** — a description you run later with an environment:

```rust
fn get_user(id: u64) -> Effect<User, DbError, Env>
where
    Env: Needs<DatabaseKey>,
{
    effect!(|env: &mut Env| {
        let db = require!(env, DatabaseKey);
        let user = ~ db.get_user(id);
        user
    })
}
```

The database client is no longer a function parameter. It's declared via `Needs<DatabaseKey>` and retrieved with `require!`. The business logic is identical; what changes is how dependencies are supplied at `run_with` / `main`.

## Pattern 1: async fn → fn returning Effect

**Before**

```rust
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

```rust
pub fn process_order(order_id: OrderId) -> Effect<Receipt, AppError, Env>
where
    Env: Needs<DatabaseKey> + Needs<MailerKey>,
{
    effect!(|env: &mut Env| {
        let db     = require!(env, DatabaseKey);
        let mailer = require!(env, MailerKey);
        let order   = ~ db.get_order(order_id);
        let receipt = ~ db.complete_order(order);
        ~ mailer.send_receipt(&receipt);
        receipt
    })
}
```

**Migration steps:**

1. Remove dependency parameters (`db`, `mailer`)
2. Add `where Env: Needs<K> + …` (or generic `R: Needs<K>`)
3. Replace `async move { … }` with `effect!(|env: &mut Env| { … })`
4. Replace `.await?` with `~ ` prefix
5. Use `require!(env, K)` for each service
6. Wire providers at the edge with `run_with([provide!(…), …], effect)`

## Pattern 2: Wrapping Third-Party Async

Third-party libraries return `Future`s, not `Effect`s. Use `from_async` to wrap them:

**Before**

```rust
async fn fetch_price(symbol: &str) -> Result<f64, reqwest::Error> {
    reqwest::get(format!("https://api.example.com/price/{symbol}"))
        .await?
        .json::<PriceResponse>()
        .await
        .map(|r| r.price)
}
```

**After**

```rust
fn fetch_price(symbol: String) -> Effect<f64, reqwest::Error, ()> {
    from_async(move |_env| async move {
        reqwest::get(format!("https://api.example.com/price/{symbol}"))
            .await?
            .json::<PriceResponse>()
            .await
            .map(|r| r.price)
    })
}
```

The `from_async` closure still uses `.await` internally. Only the outermost function signature changes.

## Pattern 3: Error Types

**Before** — single monolithic error enum

```rust
#[derive(Debug)]
enum AppError {
    DbError(DbError),
    MailError(MailError),
    NotFound(String),
}
```

**After** — effects propagate errors through `E`

```rust
#[derive(Debug)] struct NotFoundError(String);

fn get_user(id: u64) -> Effect<User, DbError, Env>
where
    Env: Needs<DatabaseKey>,
{ /* … */ }
```

You still need an `AppError` at the top level (in `main` or your HTTP handler), but individual functions no longer need unrelated error variants.

## Pattern 4: Shared State

**Before** — `Arc<Mutex<T>>` passed through function calls

```rust
async fn handler(state: Arc<Mutex<AppState>>) -> Response {
    let mut s = state.lock().unwrap();
    s.request_count += 1;
    // …
}
```

**After** — shared state as a capability

```rust
define_capability!(AppStateKey, Arc<Mutex<AppState>>);

fn handler() -> Effect<Response, AppError, Env>
where
    Env: Needs<AppStateKey>,
{
    effect!(|env: &mut Env| {
        let state = require!(env, AppStateKey);
        let mut s = state.lock().unwrap();
        s.request_count += 1;
        // …
    })
}
```

Or, for transactional mutable state across fibers, use `TRef` + STM (see Part III).

## Pattern 5: Resource Cleanup

**Before** — manual `drop` or relying on `Drop` impls

```rust
async fn with_connection<F, T>(pool: &Pool, f: F) -> Result<T, DbError>
where F: AsyncFnOnce(&Connection) -> Result<T, DbError>
{
    let conn = pool.get().await?;
    let result = f(&conn).await;
    result
}
```

**After** — explicit `Scope`

```rust
fn with_connection<F, A, E>(f: F) -> Effect<A, E, Env>
where
    F: FnOnce(&Connection) -> Effect<A, E, Env> + 'static,
    Env: Needs<PoolKey>,
    E: From<DbError> + 'static,
    A: 'static,
{
    effect!(|env: &mut Env| {
        let pool = require!(env, PoolKey);
        ~ scope.acquire(
            pool.get(),
            |conn| pool.release(conn),
            |conn| f(conn),
        )
    })
}
```

The `Scope` finalizer runs whether the inner effect succeeds, fails, or is cancelled.

## HTTP boundaries: raw `reqwest` → workspace crates

After effects replace bare `async fn`, move HTTP edges toward **typed capabilities**:

1. **`id_effect_platform`** — `HttpClientKey` + `ReqwestHttpClientProvider` + `execute` for portable requests.
2. **`id_effect_reqwest`** — `reqwest::Client` keyed in `Env`, pools, `json_schema` — see [HTTP via reqwest](./part2/ch07-07-reqwest-http.md).

Host either style under **Axum** with **`id_effect_axum`** ([Axum host](./part2/ch07-08-axum-host.md)).

## Migration Strategy

Migrate gradually, one module at a time:

1. Start with leaf functions (no effect dependencies yet).
2. Move up the call graph.
3. Push `run_with` / `run_blocking` to `main` or the request handler.
4. Convert tests last — swap `provide!(Mock…)` implementations.

You can mix async functions and effects during the transition: wrap async with `from_async`; call effects with `run_blocking` or `run_async` at boundaries.

---

## Migrating v1 DI to capability DI v2

id_effect v2 replaces the Effect.ts-style tag/HList stack with **capability keys**, **`Env`**, and **`ProviderSpec`**. v1 symbols (`service_key!`, `ctx!`, `req!`, `Layer`/`Stack`, `.provide()` on effects) are removed from the public DI path.

### Symbol mapping

| v1 (removed from DI path) | v2 |
|---------------------------|-----|
| `service_key!(K: V)` | `define_capability!(K, V)` |
| `Tagged<K>` / `tagged(v)` | `Env::insert::<K>(v)` |
| `Context` / `Cons` / `Nil` / `ctx!(…)` | `Env` (order-independent) |
| `Get<K>` / `NeedsX` supertraits | `Needs<K>` |
| `~ ServiceKey` in `effect!` | `require!(env, K)` |
| `req!(K: V \| …)` | `caps!(…)` + `Needs<K>` bounds |
| `Layer` / `Stack` / `layer_service` | `ProviderSpec` + `provide!(P)` |
| `effect.provide(ctx)` | `run_with([provide!(…)], effect)` |
| `LayerGraph` (app wiring) | `CapabilityGraph` (via `run_with` / `build_env`) |

### Example: service key → capability key

**v1**

```rust
service_key!(UserRepoKey: Arc<dyn UserRepository>);

fn get_user<R: NeedsUserRepo>(id: u64) -> Effect<User, DbError, R> {
    effect! {
        let repo = ~ UserRepoKey;
        ~ repo.get_user(id)
    }
}

run_blocking(get_user(1).provide(ctx!(tagged::<UserRepoKey>(repo))))?;
```

**v2**

```rust
define_capability!(UserRepoKey, Arc<dyn UserRepository>);

fn get_user(id: u64) -> Effect<User, DbError, Env>
where
    Env: Needs<UserRepoKey>,
{
    effect!(|env: &mut Env| {
        let repo = require!(env, UserRepoKey);
        ~ repo.get_user(id)
    })
}

run_with([provide!(UserRepoLive)], get_user(1))?;
```

### Example: layer stack → provider list

**v1**

```rust
let app_layer = config_layer
    .stack(db_layer)
    .stack(user_repo_layer);

run_blocking(my_app().provide_layer(app_layer))?;
```

**v2**

```rust
run_with(
    [
        provide!(ConfigLive),
        provide!(DatabaseLive),
        provide!(UserRepoLive),
    ],
    my_app(),
)?;
```

Each `*Live` type implements `ProviderSpec`. Dependencies are declared in `requires()` and satisfied via `deps.get::<K>()` inside `provide()`. [`CapabilityGraph`](../../src/capability/graph.rs) plans build order — no manual stacking.

### Test environments

**v1**

```rust
let env = ctx!(tagged::<DatabaseKey>(mock_db), tagged::<LoggerKey>(mock_log));
run_blocking(effect.provide(env))?;
```

**v2**

```rust
let mut env = Env::new();
env.insert::<DatabaseKey>(mock_db);
env.insert::<LoggerKey>(mock_log);
run_blocking(effect, env)?;

// or
run_with([provide!(MockDatabaseLive), provide!(MockLoggerLive)], effect)?;
```

### Migration checklist

1. Replace each `service_key!` with `define_capability!(K, V)`.
2. Change `NeedsX` / `Get<K>` bounds to `Needs<K>`.
3. Replace `~ K` with `require!(env, K)` (add `|env: &mut Env|` to `effect!` when needed).
4. Replace layer stacks with `ProviderSpec` impls + `provide!(…)`.
5. Replace `.provide()` / `.provide_layer()` with `run_with` or manual `Env` + `run_blocking`.
6. Update `main`, tests, and workspace crate providers (`id_effect_platform`, `id_effect_config`, etc.) in the same release — v2 is a clean break.

See Part II (chapters 4–7) for the full v2 narrative.
