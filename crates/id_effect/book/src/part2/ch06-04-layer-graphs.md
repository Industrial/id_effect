# Layer Graphs — Wiring a Whole Application

An application graph is an expression built from layers and the three combinators. Its type says
everything it produces and everything it still needs, and the compiler checks every connection.

## Declaring the graph

```rust
use id_effect::{HCons, HNil, Layer, caps};
# use id_effect::{hlist, need, sync};
# struct Config; struct Db; struct Cache; struct UserRepo; struct AppError;

fn config_live() -> Layer<caps!(Config), AppError> {
  // Layer::effect: load from the env
#   Layer::effect(|| sync(|| hlist![Config]))
}
fn db_live() -> Layer<caps!(Db), AppError, caps!(Config)> {
  // Layer::acquire_release
#   Layer::acquire_release(|| need::<Config>().map(|_| hlist![Db]), |_, _| sync(|| ()))
}
fn cache_live() -> Layer<caps!(Cache), AppError, caps!(Config)> {
  // ...
#   Layer::effect(|| need::<Config>().map(|_| hlist![Cache]))
}
fn user_repo_live() -> Layer<caps!(UserRepo), AppError, caps!(Db)> {
  // ...
#   Layer::effect(|| need::<Db>().map(|_| hlist![UserRepo]))
}

fn app_layer() -> Layer<caps!(Config, Db, Cache, UserRepo), AppError> {
  let config = config_live(); // one layer value, shared below
  let data = db_live().merge(cache_live()).provide_merge(config);
  user_repo_live().provide_merge(data)
}
```

There is no separate planner: build order follows the expression (`provide_merge` builds its base
first, `merge` its left side first), and memoisation makes a shared layer build once.

## What the compiler checks

- **Missing capabilities**: feeding a layer whose output lacks something the upper layer or the
  effect needs is an unsatisfied `Embed` bound, a compile error naming both environments.
- **Cycles**: a layer can only consume capabilities that exist when it is wired, so a cycle cannot
  be expressed.
- **Unused inputs**: whatever a graph still needs stays in its `In` type; `provide_layer` on an
  effect returns an effect that needs exactly that, so you cannot forget to close the graph.

The one check left to run time is **duplicate outputs** (two layers producing the same capability),
which `merge` / `provide_merge` reject with a panic when the graph is wired, not while it runs.

## Running with the graph

```rust
# use id_effect::{Effect, Layer, caps, effect, hlist};
# struct Config; struct AppError;
# fn app_layer() -> Layer<caps!(Config), AppError> { Layer::succeed(hlist![Config]) }
# fn program() -> Effect<(), AppError, caps!(Config)> { effect!({ let _ = ~Config; }) }
fn main() {
  let exit = program().provide_layer(app_layer()).block_on();
  // ...
}
```

Long-running services that should live until shutdown use `launch`:

```rust
# use id_effect::{Effect, Layer, Never, caps, hlist};
# struct Server; struct AppError;
# fn server_layer() -> Layer<caps!(Server), AppError> { Layer::succeed(hlist![Server]) }
// Builds the graph and keeps it alive until the fiber is interrupted, then runs the finalizers.
let server: Effect<Never, AppError> = server_layer().launch();
```

and code that manages its own `Scope` builds into it with `layer.build_scoped(&scope)`.

## Conditional wiring

A layer's type is fixed, so a conditional graph chooses between layers of the **same type**:

```rust
# use id_effect::{Layer, caps, hlist};
# struct Metrics { enabled: bool } struct AppError;
# fn metrics_live() -> Layer<caps!(Metrics), AppError> { Layer::succeed(hlist![Metrics { enabled: true }]) }
# fn metrics_noop() -> Layer<caps!(Metrics), AppError> { Layer::succeed(hlist![Metrics { enabled: false }]) }
fn metrics_layer(enabled: bool) -> Layer<caps!(Metrics), AppError> {
  if enabled { metrics_live() } else { metrics_noop() }
}
```

Both branches produce `Metrics`; the no-op implementation simply records nothing.

## When to use what

| Situation | Prefer |
|-----------|--------|
| one or two values, tests | `.provide(..)` / `.provide_env(hlist![..])` |
| constructions with inputs, failures or resources | `Layer` graph + `provide_layer` |
| a service alive until shutdown | `layer.launch()` |
| a caller-managed lifetime | `layer.build_scoped(&scope)` |
| capabilities only known at run time (plugins) | `DynEnv` + `try_need` |
