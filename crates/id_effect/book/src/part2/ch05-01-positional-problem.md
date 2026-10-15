# The Problem with Positional Types

A first attempt at an environment type is a tuple. That works briefly, then becomes fragile.

## The tuple explosion

```rust
Effect<A, E, (Database, Logger)>                           // readable
Effect<A, E, (Pool, Pool, Logger, Config, HttpClient)>     // which Pool is which?
```

## Fragility under change

```rust
fn foo() -> Effect<A, E, (Database, Logger)>          // 0, 1
fn foo() -> Effect<A, E, (Database, Config, Logger)>  // a teammate inserts Config
```

Every caller that built `(db, log)` must now build `(db, config, log)`, and every `env.1` silently
means something else.

## Same-type collision

```rust
run((cache_pool, main_pool)); // compiles, wrong at runtime
```

## What id_effect does instead

`R` is a type-level list, `HCons<A, HCons<B, HNil>>`, so it has an order, but **no code depends on
that order**:

- a lookup (`~Database`, `need::<Database>()`) finds a capability **by its type**, wherever it is
  in the list (the compiler infers its position);
- `provide::<T>` removes `T` wherever it is, and `provide_env` accepts any environment that
  contains every capability, in any order;
- a `~` bind lifts a sub-effect into any environment that contains its capabilities.

Inserting `Config` in the middle of `caps!(Database, Config, Logger)` therefore changes nothing for
the code that reads `Database` or `Logger`.

The type is the identity, so two capabilities of the same Rust type cannot share an `R`
(`caps!(Pool, Pool)` is a compile error). Give each a name with a newtype:

```rust
struct MainDb(Pool);
struct CacheDb(Pool);

fn report() -> Effect<Report, DbError, caps!(MainDb, CacheDb)> { /* ... */ }
```

Swapping them is now a type error, not a runtime surprise.
