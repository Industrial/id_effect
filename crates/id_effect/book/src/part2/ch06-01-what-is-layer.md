# What Is a Layer?

An `Effect` describes a computation that *needs* an environment. A `Layer` describes how to
*build* part of one.

```text
Effect<User, DbError, caps!(Database)>
  └── "I need a Database to produce a User"

Layer<caps!(Database), DbError, caps!(Config)>
  └── "I need a Config to produce a Database (and may fail with DbError)"
```

Effects declare needs; layers declare construction.

## The type

`Layer<Out, E, In = HNil>`:

- **`Out`**: the capabilities it produces, an HList such as `caps!(Database)`;
- **`E`**: the error its construction can fail with;
- **`In`**: the capabilities its construction needs.

```rust
use id_effect::{Effect, HCons, HNil, Layer, hlist, need};

struct Config(&'static str);
struct Db(&'static str);

fn db() -> Layer<HCons<Db, HNil>, (), HCons<Config, HNil>> {
  Layer::effect(|| need::<Config>().map(|c| hlist![Db(c.0)]))
}
```

The construction is an ordinary effect over `In` that returns the `Out` HList.

## Layers are lazy recipes

Making a layer runs nothing. Construction happens when the layer is **built**:

| Owner | Builds | Releases (finalizers run) |
|-------|--------|---------------------------|
| `eff.provide_layer(layer)` | each time the effect runs | after the effect ends (success, failure, defect or interrupt) |
| `layer.launch()` | when the launched effect runs | when it is interrupted |
| `layer.build_scoped(&scope)` | into a caller's `Scope` | when the caller closes the scope |
| `layer.build()` | as an effect returning `Out` | only if the build fails; use an owner above for resources |

## Memoised within one build

A `Layer` is cheap to clone, and every clone is the same layer: within **one build**, a layer
used in several places is constructed once and everyone receives the same `Arc`s. Separate builds
construct again, and `layer.fresh()` opts one layer out of the sharing.

## Resources and cleanup

A layer can own a resource: `Layer::acquire_release(acquire, release)` registers `release` in the
build's scope, and `Layer::scoped(|scope| ..)` registers any finalizers. Finalizers are effects
(they may be asynchronous) and run once, uninterruptibly, in reverse order of acquisition, when the
build's owner ends.

## The key insight

- **Effects** are programs: *what to do* with capabilities.
- **Layers** are constructors: *how to build* capabilities, and how to release them.

Both are values, both are typed, and the compiler checks that they fit together.
