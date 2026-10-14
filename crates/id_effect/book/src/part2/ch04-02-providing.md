# Providing Dependencies — `run_with` and `Env`

An effect with `R = caps!(…)` cannot run until its capabilities exist. **Provide at the edge**, not inside library code.

## `run_with` — the main entrypoint

```rust
use id_effect::{Effect, ProviderSpecDerive, caps, effect, provide, require, run_with, succeed};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counter(pub u32);

#[derive(ProviderSpecDerive)]
#[provides(Counter, with = || Counter(42))]
struct CounterLive;

fn app() -> Effect<u32, (), caps!(Counter)> {
    effect!(|r| {
        let counter = ~Counter;
        counter.0
    })
}

fn main() {
    let n = run_with([provide!(CounterLive)], app()).expect("run");
}
```

[`provide!`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/provider.rs) wraps a [`ProviderSpec`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/provider.rs) as a [`ProviderBox`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/provider.rs). [`run_with`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/run.rs) collects providers, plans build order via [`CapabilityGraph`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/graph.rs), and runs the effect.

## Provider constructors — `with = …`

`#[provides(Service, with = …)]` tells the derive how to build the service:

| `with =` | Generated body |
|----------|----------------|
| `\|\| expr` | `expr` |
| `\|deps: &Env\| expr` | `expr`, with `deps` bound to the already-built capabilities |
| `Self::build` (any path) | `Self::build()` |

Without `with`, the derive falls back to `Self::default()` (when the struct derives `Default`) or `Self::new()`. That older form needs `impl CounterLive { fn new() -> Counter { … } }` — a `new` that does not return `Self`, which trips `clippy::new_ret_no_self`:

```rust
// Before (0.5): an impl block and a clippy allow per provider
#[derive(ProviderSpecDerive)]
#[provides(Counter)]
struct CounterLive;

impl CounterLive {
    #[allow(clippy::new_ret_no_self)]
    fn new() -> Counter {
        Counter(42)
    }
}

// After (0.6): no impl block, nothing to allow
#[derive(ProviderSpecDerive)]
#[provides(Counter, with = || Counter(42))]
struct CounterLive;
```

Longer constructors keep their own function, named for what it does: `#[provides(Pool, with = Self::connect)]` plus `impl PoolLive { fn connect() -> Pool { … } }`. Provider structs are unit markers referenced only as types (`provide!(CounterLive)`); they do not need `#[allow(dead_code)]`.

## Multiple providers

Pass every provider the app needs in one list — order in the array does not matter; the graph topologically sorts by `requires()`:

```rust
run_with(
    [
        provide!(ConfigLive),
        provide!(DatabaseLive),
        provide!(LoggerLive),
        provide!(UserRepoLive),
    ],
    my_application(),
)?;
```

## Manual `Env` for tests

When you only need a handful of values, build `Env` by hand:

```rust
let mut env = Env::new();
env.insert::<Cap<Database>>(mock_pool);
env.insert::<Cap<EffectLogger>>(test_logger);

let user = run_blocking(get_user(42), env)?;
```

[`build_env`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/capability/run.rs) is the middle ground — same provider types as production, but you get the `Env` back without running an effect:

```rust
let env = build_env([provide!(MockUserRepoLive)])?;
run_test(get_user(1), env)?;
```

## Where to provide

**Provide at `main`, test setup, or HTTP/Tokio boundaries — not inside library functions.**

```rust
// BAD — library reaches for concrete deps
pub fn process_order(order: Order) -> Effect<Receipt, AppError, ()> {
    let db = connect("hardcoded-url");
    // ...
}

// GOOD — library declares needs; caller wires them
pub fn process_order(order: Order) -> Effect<Receipt, AppError, caps!(Database, EffectLogger)> {
    // ...
}
```

## Summary

| API | Purpose |
|-----|---------|
| `run_with([provide!(P), …], effect)` | Build graph + run |
| `build_env([provide!(P), …])` | Build `Env` only |
| `Env::insert::<Cap<K>>(value)` | Manual test wiring |
| `run_blocking(effect, env)` | Run when `Env` is already built |

None of these execute effect steps until `run_with` / `run_blocking` / `run_async` is called — wiring stays lazy until the boundary.
