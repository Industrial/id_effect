# Pools — Reusing Expensive Resources

Creating a database connection takes time: DNS lookup, TCP handshake, TLS, authentication. Creating one per request is wasteful. A pool maintains a set of connections and lends them out, returning them when done.

id_effect provides `Pool` and `KeyedPool` as first-class effect constructs.

## Pool: Basic Connection Pool

```rust
use id_effect::Pool;

// Create a pool of up to 10 connections
let pool: Pool<Connection> = Pool::new(
    || open_connection("postgres://localhost/app"),  // factory
    10,                                               // max size
);
```

The pool lazily creates connections up to the max. Idle connections are kept alive for reuse.

## Using a Pool Connection

```rust
pool.with_resource(|conn: &Connection| {
    effect! {
        let rows = ~ conn.query("SELECT * FROM users");
        rows.into_iter().map(User::from_row).collect::<Vec<_>>()
    }
})
```

`with_resource` acquires a connection from the pool, runs the effect, and returns the connection automatically when done — regardless of success, failure, or cancellation. No `acquire_release` boilerplate; the pool handles it.

## Waiting for Availability

If all connections are in use, `with_resource` waits until one becomes available:

```rust
// Concurrent requests share the pool; each waits its turn
fiber_all(vec![
    pool.with_resource(|c| query_a(c)),
    pool.with_resource(|c| query_b(c)),
    pool.with_resource(|c| query_c(c)),
])
```

The pool queues waiters and notifies them as connections are returned.

## KeyedPool: Multiple Named Pools

For scenarios with multiple distinct pools (e.g., read replica + write primary):

```rust
use id_effect::KeyedPool;

let pools: KeyedPool<&str, Connection> = KeyedPool::new(
    |key: &&str| open_connection(key),
    5,  // max per key
);

// Get a connection for the write primary
pools.with_resource("write-primary", |conn| { ... })

// Get a connection for the read replica
pools.with_resource("read-replica", |conn| { ... })
```

Each key has its own independently bounded pool.

## Pool as a capability

In practice, pools live in [`Env`](../../src/capability/env.rs) and are wired at the program edge:

```rust
use id_effect::{Effect, caps, effect, provide, require, run_with, Needs, ProviderSpecDerive};
struct DbPool;

#[derive(ProviderSpecDerive)]
#[provides(DbPool)]
struct DbPoolLive;

impl DbPoolLive {
    fn new(config: &Config) -> Pool<Connection> {
        Pool::new(|| Connection::open(config.db_url()), config.pool_size())
    }
}

fn query_users() -> Effect<Vec<User>, DbError, caps!(DbPool)> {
    effect!(|r| {
        let pool = ~DbPool;
        ~ pool.with_resource(|conn| {
            effect!(|r| {
                let rows = ~ conn.query("SELECT * FROM users");
                rows.iter().map(User::from_row).collect::<Vec<_>>()
            })
        })
    })
}

// main or test
run_with([provide!(ConfigLive), provide!(DbPoolLive)], query_users())?;
```

Pool creation runs in the provider graph; business code declares `caps!(DbPool)` and uses `~DbPool` inside `effect!`.
