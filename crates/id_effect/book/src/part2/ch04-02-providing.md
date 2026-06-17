# Providing Dependencies — `run_with` and `Env`

An effect with `R = Env` (or a generic `R: Needs<K>`) cannot run until its capabilities exist. In v2 you **provide at the edge**, not inside library code.

## `run_with` — the main entrypoint

```rust
use id_effect::{Effect, Env, ProviderError, ProviderSpec, define_capability, provide, require, run_with};

define_capability!(CounterKey, Counter);

struct CounterLive;

impl ProviderSpec for CounterLive {
    type Key = CounterKey;
    type Output = Counter;

    fn provider_id() -> &'static str { "counter-live" }

    fn provide(_deps: &Env) -> Result<Counter, ProviderError> {
        Ok(Counter(42))
    }
}

fn app() -> Effect<u32, (), Env> {
    Effect::new(|env: &mut Env| {
        let counter = require!(env, CounterKey);
        Ok(counter.0)
    })
}

fn main() {
    let n = run_with([provide!(CounterLive)], app()).expect("run");
}
```

[`provide!`](../../src/capability/provider.rs) wraps a [`ProviderSpec`](../../src/capability/provider.rs) as a [`ProviderBox`](../../src/capability/provider.rs). [`run_with`](../../src/capability/run.rs) collects providers, plans build order via [`CapabilityGraph`](../../src/capability/graph.rs), and runs the effect.

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
env.insert::<DatabaseKey>(mock_pool);
env.insert::<LoggerKey>(test_logger);

let user = run_blocking(get_user(42), env)?;
```

[`build_env`](../../src/capability/run.rs) is the middle ground — same provider types as production, but you get the `Env` back without running an effect:

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
pub fn process_order(order: Order) -> Effect<Receipt, AppError, Env>
where
    Env: Needs<DatabaseKey> + Needs<LoggerKey>,
{
    // ...
}
```

## Summary

| API | Purpose |
|-----|---------|
| `run_with([provide!(P), …], effect)` | Build graph + run |
| `build_env([provide!(P), …])` | Build `Env` only |
| `Env::insert::<K>(value)` | Manual test wiring |
| `run_blocking(effect, env)` | Run when `Env` is already built |

None of these execute effect steps until `run_with` / `run_blocking` / `run_async` is called — wiring stays lazy until the boundary.
