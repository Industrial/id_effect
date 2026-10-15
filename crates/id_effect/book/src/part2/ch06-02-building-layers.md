# Building Layers — From Simple to Complex

## A ready value: `Layer::succeed`

```rust
use id_effect::{HCons, HNil, Layer, hlist};

struct Config(&'static str);

fn config() -> Layer<HCons<Config, HNil>, ()> {
  Layer::succeed(hlist![Config("mem://")])
}
```

It needs nothing (`In = HNil`) and every build hands out the same `Arc`s.

## An effectful construction: `Layer::effect`

`Layer::effect(make)` calls `make()` once per build; the effect it returns may read the layer's
input and may fail:

```rust
use id_effect::{Effect, Exit, HCons, HNil, Layer, Never, caps, effect, hlist, sync};

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppConfig(String);

fn config_layer() -> Layer<HCons<AppConfig, HNil>, Never> {
  Layer::effect(|| sync(|| hlist![AppConfig("loaded-from-effect".to_string())]))
}

fn app() -> Effect<String, Never, caps!(AppConfig)> {
  effect!({ ~AppConfig.0.clone() })
}

let cfg = app().provide_layer(config_layer()).block_on();
assert_eq!(cfg, Exit::succeed("loaded-from-effect".to_string()));
```

A **dependent** layer reads its input like any effect:

```rust
struct Db(&'static str);

fn db() -> Layer<HCons<Db, HNil>, (), HCons<Config, HNil>> {
  Layer::effect(|| need::<Config>().map(|c| hlist![Db(c.0)]))
}
```

`Layer::from_effect(eff)` wraps a one-shot effect instead; since effects run once, such a layer can
be built only once (a second build dies with a defect). Prefer `Layer::effect`.

## Resources: `Layer::acquire_release` and `Layer::scoped`

```rust
use id_effect::{HCons, HNil, Layer, hlist, sync};

struct Pool;

let pool: Layer<HCons<Pool, HNil>, ()> = Layer::acquire_release(
  || sync(|| hlist![Pool]),             // acquire, uninterruptibly
  |_pool, _exit_kind| sync(|| close()), // release: an Effect<(), Never, HNil>
);
```

`release` is registered in the build's scope as soon as `acquire` succeeds and runs once when the
scope closes, fed the closing `ExitKind`. `Layer::scoped(|scope| ..)` is the general form: the
construction receives the build's `Scope` and can add any finalizers to it
(`scope.add_finalizer(..)`).

## The provider derive

For a capability built from a constructor with no inputs, `#[derive(ProviderSpecDerive)]` writes
the layer: it emits an inherent `layer()`, a `Layer<HCons<Service, HNil>, E>` that needs nothing and
never fails (`E` is whatever the graph it joins uses).

```rust
use std::sync::Arc;
use id_effect::{Effect, HCons, HNil, Never, ProviderSpecDerive, need, sync};

#[derive(Clone)]
pub struct Counter(u32);

#[derive(ProviderSpecDerive)]
#[provides(Counter, with = || Counter(42))]
pub struct CounterLive;

#[derive(Clone)]
pub struct Pool(&'static str);

fn close_pool(pool: Arc<Pool>) -> Effect<(), Never, HNil> {
  sync(move || drop(pool))
}

#[derive(ProviderSpecDerive)]
#[provides(Pool, with = || Pool("mem://"), release = close_pool)]
pub struct PoolLive;

fn read_counter() -> Effect<u32, (), HCons<Counter, HNil>> {
  need::<Counter>().map(|c| c.0)
}

let read: Effect<u32, ()> = read_counter().provide_layer(CounterLive::layer());
```

- `with = || expr` or `with = Self::build` (a path called with no arguments) builds the service;
  without `with`, the derive uses `Self::default()` (when the struct derives `Default`) or
  `Self::new()`;
- `release = f` makes it an `acquire_release` layer whose finalizer `f(Arc<Service>)` runs when
  the layer's scope closes.

A capability whose construction needs other capabilities, or can fail, is a hand-written
`Layer::effect` / `Layer::acquire_release`.

## Test doubles

A test double is just another layer (or a plain value) for the same capability:

```rust
fn user_repo_mock() -> Layer<HCons<UserRepo, HNil>, AppError> {
  Layer::succeed(hlist![UserRepo(Arc::new(InMemoryUsers::fixture()))])
}
```

Business logic keeps `caps!(UserRepo)`; only the wiring at the edge changes. For one-off tests,
`.provide(UserRepo(..))` is often simpler than a layer.

## The pattern in practice

```text
config()            (needs nothing)
  → db()            (needs Config)
  → cache()         (needs Config)
  → user_repo()     (needs Db)
```

The next section composes these into one graph.
