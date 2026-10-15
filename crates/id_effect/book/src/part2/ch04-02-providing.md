# Providing Dependencies — provide, provide_env and Layers

An effect with `R = caps!(..)` cannot run until its capabilities are provided. **Provide at the
edge** (`main`, a test, a request handler), not inside library code. Every `provide*` method
returns a new effect with a smaller `R`; once `R` is `HNil` the effect can be run.

## `provide` — one capability

```rust
use id_effect::{Effect, Exit, Never, caps, effect};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Counter(pub i32);

fn program() -> Effect<i32, Never, caps!(Counter)> {
  effect!({ ~Counter.0 + 1 })
}

assert_eq!(program().provide(Counter(41)).block_on(), Exit::succeed(42));
```

`provide::<T>` removes exactly `T` from `R`, wherever it sits; the rest stays required, so you can
provide step by step:

```rust
use id_effect::{Effect, HCons, HNil, need};

struct Db(u32);
struct Logger;

fn query() -> Effect<u32, (), HCons<Db, HCons<Logger, HNil>>> {
  need::<Db>().map(|db| db.0)
}

let partly: Effect<u32, (), HCons<Logger, HNil>> = query().provide(Db(7));
let runnable: Effect<u32, ()> = partly.provide(Logger);
```

Providing a capability the effect does not need is a compile error (a type mismatch at the
`.provide(..)` argument).

## `provide_env` — a whole environment

`provide_env` takes any HList value that **contains** every capability of `R`, in any order, with
extras allowed. It is the natural fit for an application environment built once:

```rust
use id_effect::hlist;

let app_env = hlist![Cache(0), Logger("b"), Db(2)];
let exit = describe().provide_env(app_env).block_on(); // describe() needs Db and Logger
```

Related methods:

| Method | Provides |
|--------|----------|
| `.provide(t)` | one capability `T` |
| `.provide_some(hlist![a, b])` | a sub-list, in any order |
| `.provide_all(env)` | exactly `R` |
| `.provide_env(env)` | any environment containing `R` |
| `.provide_layer(layer)` | the output of a [`Layer`](./ch06-00-layers.md), built around the effect |

## Layers and the provider derive

When capabilities have to be *constructed* (opened, loaded, connected), describe the construction
as a `Layer` and provide it with `provide_layer`. The derive `ProviderSpecDerive` writes the
simple ones for you: it emits an inherent `layer()`:

```rust
use id_effect::{Effect, Exit, Never, caps, effect};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counter(pub u32);

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(Counter, with = || Counter(42))]
struct CounterLive;

fn app() -> Effect<u32, Never, caps!(Counter)> {
  effect!({ ~Counter.0 })
}

let exit = app().provide_layer(CounterLive::layer()).block_on();
assert_eq!(exit, Exit::succeed(42));
```

`#[provides(Service, with = ..)]` takes `|| expr` or a path called with no arguments (such as
`Self::connect`). Without `with`, the derive uses `Self::default()` (when the struct derives
`Default`) or `Self::new()`. `#[provides(Service, with = .., release = close)]` adds a finalizer
`close(Arc<Service>) -> Effect<(), Never, HNil>` that runs when the layer's scope closes.
`CounterLive::layer()` needs nothing and never fails, so its error type is whatever the graph it
joins uses.

Independent layers combine with `merge`; a layer that needs another one's output is fed with
`provide_to` / `provide_merge` (Chapter 6):

```rust
let exit = my_application()
  .provide_layer(ConfigLive::layer().merge(DatabaseLive::layer()).merge(LoggerLive::layer()))
  .block_on();
```

## Where to provide

**Provide at `main`, in test setup, or at framework boundaries, not inside library functions.**

```rust
// BAD: the library reaches for concrete dependencies
pub fn process_order(order: Order) -> Effect<Receipt, AppError> {
  let db = Database::connect("hardcoded-url");
  // ...
}

// GOOD: the library declares its needs; the caller wires them
pub fn process_order(order: Order) -> Effect<Receipt, AppError, caps!(Database, AuditLog)> {
  // ...
}
```

Providing is lazy too: nothing is built or run until the provided effect itself is run.
