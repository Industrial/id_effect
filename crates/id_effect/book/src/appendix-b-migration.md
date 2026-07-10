# Migrating from `async fn` to effects

> **2.x → 3.0:** See [Migrating 2.x → 3.0](#migrating-2x--30) for the DI maturity breaking changes.



This appendix is a practical guide for converting existing async Rust code to id_effect. It covers common patterns and their id_effect equivalents, with migration steps for each.

> **1.x DI:** If you are migrating from id_effect 1.x tag/HList DI (`service_key!`, `ctx!`, `Layer`/`Stack`), skip to [Migrating 1.x DI to id_effect 3.0](#migrating-1x-di-to-id_effect-30) first.

## The Mental Model Shift

In typical async Rust, a function returns a `Future`; when that future is awaited, the work runs:

```rust
async fn get_user(id: u64, db: &DbClient) -> Result<User, DbError> {
    db.query_one("SELECT * FROM users WHERE id = $1", &[&id]).await
}
```

In id_effect, domain functions return an **`Effect`** — a description you run later with an environment:

```rust
struct Database;

fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)> {
    effect!(|r: &mut caps!(Database)| {
        let db = require!(Database);
        let user = ~ db.get_user(id);
        user
    })
}
```

The database client is no longer a function parameter. It is declared via `caps!(Database)` and retrieved with `require!`. The business logic is identical; what changes is how dependencies are supplied at `run_with` / `main`.

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
struct Database;
struct Mailer;

pub fn process_order(order_id: OrderId) -> Effect<Receipt, AppError, caps!(Database, Mailer)> {
    effect!(|r: &mut caps!(Database, Mailer)| {
        let db     = require!(Database);
        let mailer = require!(Mailer);
        let order   = ~ db.get_order(order_id);
        let receipt = ~ db.complete_order(order);
        ~ mailer.send_receipt(&receipt);
        receipt
    })
}
```

**Migration steps:**

1. Remove dependency parameters (`db`, `mailer`)
2. Declare service types (`struct Counter(u32);` or `type Database = Arc<dyn DbClient>;`)
3. Use `Effect<_, _, caps!(K1, K2)>` (or `where Env: Needs<K> + …` when generic)
4. Replace `async move { … }` with `effect!(|r: &mut caps!(…)| { … })`
5. Replace `.await?` with `~ ` prefix
6. Use `require!(K)` for each capability inside `effect!`
7. Wire providers at the edge with `run_with([provide!(…), …], effect)`

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
struct Database;

fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)> {
    effect!(|r: &mut caps!(Database)| {
        let db = require!(Database);
        ~ db.get_user(id)
    })
}
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
struct AppStateCap;

fn handler() -> Effect<Response, AppError, caps!(AppStateCap)> {
    effect!(|r: &mut caps!(AppStateCap)| {
        let state = require!(AppStateCap);
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
struct PoolCap;

fn with_connection<F, A, E>(f: F) -> Effect<A, E, caps!(PoolCap)>
where
    F: FnOnce(&Connection) -> Effect<A, E, caps!(PoolCap)> + 'static,
    E: From<DbError> + 'static,
    A: 'static,
{
    effect!(|r: &mut caps!(PoolCap)| {
        let pool = require!(PoolCap);
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

1. **`id_effect_platform`** — `HttpClientService` + `ReqwestHttpClientProvider` + `execute` for portable requests.
2. **`id_effect_platform::http::reqwest`** — `reqwest::Client` keyed in `Env`, pools, `json_schema` — see [HTTP via reqwest](./part2/ch07-07-reqwest-http.md).

Host either style under **Axum** with **`id_effect_axum`** ([Axum host](./part2/ch07-08-axum-host.md)).

## Migration Strategy

Migrate gradually, one module at a time:

1. Start with leaf functions (no effect dependencies yet).
2. Move up the call graph.
3. Push `run_with` / `run_blocking` to `main` or the request handler.
4. Convert tests last — swap `provide!(Mock…)` implementations.

You can mix async functions and effects during the transition: wrap async with `from_async`; call effects with `run_blocking` or `run_async` at boundaries.

---


## Migrating 2.x → 3.0

| Removed (2.x) | Replacement (3.0) |
|---------------|-------------------|
| `CapEnv1…6` | `caps!(K0, K1, …)` / `CapList<(K0, K1, …)>` |
| `caps!(Key, T)` (removed) | declare service type `T` directly |
| `require!(env, K)` | `require!(K)` in `effect!` or `Needs::<K>::need(env)` |
| `ctx!`, `req!`, `service_key!` | ``, `caps!`, `build_env` |
| `Layer` / `Stack` / `Effect::provide` | `ProviderSpec` + `run_with` |
| `IntoBind` | `Needs<K>` + `require!(K)` |
| config `ambient` | `Env::scoped` / `build_env` |
| `Effect<_, _, Env>` (multi-cap public API) | `Effect<_, _, caps!(…)>` |

Run `cargo test -p id_effect --test ui_compile_fail` to see compile-fail examples for each removed symbol.


## Migrating 1.x DI to id_effect 3.0

id_effect 3.0 replaces the Effect.ts-style tag/HList stack with **capability services**, **`Env`**, and **`ProviderSpec`**. 1.x symbols (`service_key!`, `ctx!`, `req!`, `Layer`/`Stack`, `.provide()` on effects) are removed from the public DI path.

### Symbol mapping

| 1.x (removed from DI path) | 3.0 |
|---------------------------|-----|
| `service_key!(K: V)` | service type `T` / `type K = Arc<dyn Trait>` |
| `Tagged<K>` / `tagged(v)` | `Env::insert::<Cap<K>>(v)` |
| `Context` / `Cons` / `Nil` / `ctx!(…)` | `Env` (order-independent) |
| `Get<K>` / `NeedsX` supertraits | `Needs<K>` |
| `~ Service` in `effect!` | `require!(K)` |
| `req!(K: V \| …)` | `caps!(…)` + `Needs<K>` bounds |
| `Layer` / `Stack` / `layer_service` | `ProviderSpec` + `provide!(P)` |
| `effect.provide(ctx)` | `run_with([provide!(…)], effect)` |
| `LayerGraph` (app wiring) | `CapabilityGraph` (via `run_with` / `build_env`) |

### Example: service key → capability service

**1.x**

```rust
service_key!(UserRepo: Arc<dyn UserRepository>);

fn get_user<R: NeedsUserRepo>(id: u64) -> Effect<User, DbError, R> {
    effect! {
        let repo = ~ UserRepo;
        ~ repo.get_user(id)
    }
}

run_blocking(get_user(1).provide(ctx!(tagged::<UserRepo>(repo))))?;
```

**3.0**

```rust
struct UserRepo;

fn get_user(id: u64) -> Effect<User, DbError, caps!(UserRepo)> {
    effect!(|r: &mut caps!(UserRepo)| {
        let repo = require!(UserRepo);
        ~ repo.get_user(id)
    })
}

run_with([provide!(UserRepoLive)], get_user(1))?;
```

### Example: layer stack → provider list

**1.x**

```rust
let app_layer = config_layer
    .stack(db_layer)
    .stack(user_repo_layer);

run_blocking(my_app().provide_layer(app_layer))?;
```

**3.0**

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

Each `*Live` type implements `ProviderSpec`. Dependencies are declared in `requires()` and satisfied via `deps.get::<Cap<K>>()` inside `provide()`. [`CapabilityGraph`](../../src/capability/graph.rs) plans build order — no manual stacking.

### Test environments

**1.x**

```rust
let env = ctx!(tagged::<Database>(mock_db), tagged::<EffectLogger>(mock_log));
run_blocking(effect.provide(env))?;
```

**3.0**

```rust
let mut env = build_env([provide!(DatabaseLive), provide!(LoggerLive)])?;
env.insert::<Cap<Database>>(mock_db);

run_blocking(effect, env)?;

// or swap providers entirely
run_with([provide!(MockDatabase), provide!(MockLogger)], effect)?;
```

### Migration checklist

1. Declare service types (structs or `Arc<dyn Trait>` aliases).
2. Change `NeedsX` / `Get<K>` bounds to `Needs<K>` or `caps!(…)`.
3. Replace `~ K` with `require!(K)` (use `|r: &mut caps!(…)|` on `effect!` when needed).
4. Replace layer stacks with `ProviderSpec` impls + `provide!(…)`.
5. Replace `.provide()` / `.provide_layer()` with `run_with` or manual `Env` + `run_blocking`.
6. Update `main`, tests, and workspace crate providers (`id_effect_platform`, `id_effect_config`, etc.) in the same release — 3.0 is a clean break.

See Part II (chapters 4–7) for the full capability DI narrative.
