# The Problem with Positional Types

Early effect code sometimes used tuples as `R`. That works briefly, then becomes fragile.

## The tuple explosion

Two dependencies: readable.

```rust
Effect<A, E, (Database, Logger)>
```

Five dependencies: which is which?

```rust
Effect<A, E, (Pool, Pool, Logger, Config, HttpClient)>
//            ^^^^ two Pools — main DB or cache?
```

Tuples are positional. `(Pool, Pool, …)` is ambiguous when both fields share a type.

## Fragility under change

```rust
fn foo() -> Effect<A, E, (Database, Logger)>
//                        0         1
```

A teammate inserts `Config`:

```rust
fn foo() -> Effect<A, E, (Database, Config, Logger)>
//                        0         1       2
```

Every caller that built `(db, log)` must become `(db, config, log)`. The type system does not point at stale indices — it's a silent refactor hazard.

## Same-type collision

Rust cannot distinguish a main-database `Pool` from a cache `Pool` in a tuple:

```rust
run_blocking(effect, (cache_pool, main_pool)); // compiles, wrong at runtime
```

## What we need

Each dependency needs a **compile-time name** independent of position:

- `Database` → the primary `Pool`
- `Cache` → the cache `Pool`

Different keys, same underlying type — the compiler catches swaps.

That's what [``](../../src/capability/key.rs) generates.
