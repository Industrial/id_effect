# Pools — Reusing Expensive Resources

Creating a database connection takes time: DNS lookup, TCP handshake, TLS, authentication. Creating one per request is wasteful. A pool keeps a bounded set of items and lends them out, taking them back when the borrower is done.

id_effect provides `Pool` and `KeyedPool` (Effect.ts `Pool` / `KeyedPool`), in the `resource` module.

## Pool: Bounded, Reusable Items

```rust
use id_effect::{Effect, Exit, Never, Pool, succeed};

let pool: Pool<u32> = Pool::new(2, || succeed(7)); // at most 2 checked out; `acquire` builds new items
let p = pool.clone();
let eff: Effect<u32, Never> = Effect::scoped(move |scope| p.get(&scope));
assert!(matches!(eff.block_on(), Exit::Success(7)));
// The scope closed, so the item was checked back in.
assert_eq!((pool.idle_count(), pool.in_use()), (1, 0));
```

- `Pool::new(capacity, acquire)` is a plain constructor; nothing is acquired until the first checkout. `Pool<A, E = Never, R = HNil>`: `acquire` may fail with `E` and need capabilities `R`, and `get` then has the same `E` and `R`.
- `pool.get(&scope)` waits for capacity, then reuses an idle item or acquires a new one, and registers a **scope finalizer that checks it back in** — however the scope closes, interrupt included.
- Items are handed out by clone, so a pooled connection is typically an `Arc` or a cheap handle. Clones of the pool share it.

## Waiting for Availability

When every item is checked out, `get` suspends the fiber until one is checked in. Waiting is interruptible and never leaks capacity; acquiring a new item runs uninterruptibly. Concurrent borrowers simply share the pool:

```rust,ignore
let queries = for_each_par(
    ["a", "b", "c"],
    move |q| { let p = pool.clone(); Effect::scoped(move |scope| p.get(&scope).flat_map(move |c| run(c, q))) },
    Concurrency::Unbounded,
);
```

## Lifecycle Options

```rust,ignore
let pool = Pool::new(10, connect)
    .with_ttl(Duration::from_secs(300)) // idle items older than this are released, not reused
    .with_release(|conn| close(conn));  // disposes of invalidated / expired items and on shutdown
```

- `pool.invalidate(item)` marks an item unusable (e.g. after a protocol error): idle copies are released now, a checked-out copy at check-in.
- `pool.shutdown()` is an infallible `HNil` effect, so `scope.add_finalizer(pool.shutdown())` ties the pool's lifetime to a scope. After shutdown, checkouts are interrupted.
- TTLs are read from the executor clock, so they are virtual time on `TestExecutor`.

## KeyedPool: One Bounded Pool per Key

```rust
use id_effect::{Effect, KeyedPool, Never, Scope, succeed};

let pool = KeyedPool::new(2, |k: &'static str| succeed(k.len() as u32)); // 2 per key
let scope = Scope::new();
let v: Effect<u32, Never> = pool.get("answer", &scope);
assert_eq!(v.block_on().into_result().ok(), Some(6));
```

Capacity is **per key** (e.g. a read replica and a write primary each get their own bound); `KeyedPool::new_with(|key| capacity, acquire)` sets a different capacity per key.

## Pool as a Capability

In practice a pool is built once at the edge and handed to business code through `R`:

```rust,ignore
use id_effect::{Effect, Pool, caps, need};

#[derive(Clone)]
struct DbPool(Pool<Conn, DbError>);

fn query_users() -> Effect<Vec<User>, DbError, caps!(DbPool)> {
    need::<DbPool>().flat_map(|db| {
        let pool = db.0.clone();
        // `pool.get` and `select_users` need nothing (`HNil`); `narrow` lifts them into `caps!(DbPool)`.
        Effect::scoped(move |scope| pool.get(&scope).flat_map(select_users).narrow())
    })
}

// At the edge:
let exit = query_users().provide(DbPool(Pool::new(10, connect))).block_on();
```

For a pool whose lifetime should end with the application, build it in a scoped [`Layer`](../part2/ch06-00-layers.md) (`Layer::scoped` / `Layer::acquire_release`, releasing with `pool.shutdown()`) and provide the layer instead.
