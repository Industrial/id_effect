# The Environment Value — `HCons`, `HNil` and `DynEnv`

At run time an effect's environment is a **value of its `R` type**: an HList whose cells hold
`Arc`ed capabilities.

## Structure

```rust
use id_effect::{HCons, HNil, hlist};

struct Db(u32);
struct Logger(&'static str);

// The type: caps!(Db, Logger)
type DbLog = HCons<Db, HCons<Logger, HNil>>;

// The value
let env: DbLog = hlist![Db(1), Logger("app")];
// same as HCons::new(Db(1), HCons::new(Logger("app"), HNil))
```

`HCons<H, T>` holds an `Arc<H>` and the tail `T`, so cloning an environment costs one refcount
bump per capability, and every effect that reads a capability shares the same `Arc`.

## Typed lookup: `Selector`

Reading capability `T` from an environment `R` is the trait bound `R: Selector<T, I>`. The index
`I` (`Here`, `There<Here>`, …) is a type the compiler infers: it is the position of `T`, found by
type, so callers never write it. A missing capability is an unsatisfied bound, reported as

```text
error[E0277]: capability `Metrics` is not in this effect's environment
```

The related traits do the other environment operations, all resolved at compile time:

| Trait | Used by | Does |
|-------|---------|------|
| `Selector<T, I>` | `~T`, `need::<T>()` | reads `T` |
| `Plucker<T, I>` | `.provide(t)` | removes `T`, leaving `Rest` |
| `Sculptor<Sub, Is>` | `.provide_some(sub)` | removes a sub-list |
| `Embed<R, Is>` | `.provide_env(env)`, `.narrow()`, `~sub` | projects a wider environment onto `R` |
| `Concat<Rhs>` | `Layer::merge` | appends two lists |

You meet these names in compiler errors and in generic helpers
(`R: HList + Selector<Logger, I>`); application code uses `caps!`, `hlist!` and the `provide*`
methods.

## Building the environment

```rust
# use id_effect::{Effect, HCons, HNil, Layer, caps, hlist, need};
# struct Db(u32); struct Logger(&'static str);
# #[derive(Default)] struct Cache;
# fn make() -> Effect<u32, (), caps!(Db, Logger)> { need::<Db>().map(|d| d.0) }
# fn app_layer() -> Layer<HCons<Db, HCons<Logger, HNil>>, ()> { Layer::succeed(hlist![Db(1), Logger("app")]) }
# let eff = make();
// Exactly R, in order
eff.provide_all(hlist![Db(1), Logger("app")]);
# let eff = make();

// Any environment that contains R (any order, extras allowed)
eff.provide_env(hlist![Logger("app"), Cache::default(), Db(1)]);
# let eff = make();

// From a Layer graph: the layers' output HList
eff.provide_layer(app_layer());
```

## Order independence

`hlist![Db(1), Logger("x")]` and `hlist![Logger("x"), Db(1)]` are different types, but both satisfy
an effect that needs `caps!(Db, Logger)` through `provide_env`, and a lookup of `Db` works in
either. Adding a capability never changes how existing ones are read.

## `DynEnv`: runtime-checked lookup

The typed environment is the default. For plugin-style wiring whose capabilities are not known at
compile time, `DynEnv` is a `TypeId`-keyed map with **fallible** lookups. It is itself a
capability: an effect that looks things up dynamically requires `caps!(DynEnv)`.

```rust
# fn main() -> Result<(), String> {
use std::sync::Arc;
use id_effect::{DynEnv, MissingCapability};

struct Plugin(&'static str);
struct Absent;

let mut env = DynEnv::new();
env.insert(Plugin("audit"));
let p: Arc<Plugin> = env.try_get::<Plugin>().map_err(|e| e.to_string())?;
assert_eq!(p.0, "audit");
assert!(matches!(env.try_get::<Absent>(), Err(MissingCapability { .. })));
# Ok(())
# }
```

Inside an effect, `try_need::<T>()` is an `Effect<Arc<T>, MissingCapability, caps!(DynEnv)>`: it
fails with `MissingCapability` (it never panics) when `T` is absent. Use it only where the set of
capabilities really is dynamic; everywhere else a typed `R` turns the same mistake into a compile
error.
