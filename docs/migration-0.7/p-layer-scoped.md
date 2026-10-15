# Scoped layers: finalizers, `fresh`, `launch`, `derive(ProviderSpec)::layer()`

0.7's `next::Layer` can hold resources (Effect.ts `Layer.scoped`). Each build has one
`Scope`. Layers register their finalizers into it, and they run once, uninterruptibly and in
reverse order of acquisition, when the scope closes.

```rust
// 0.6: a sync hook, run by run_with after the program
impl ProviderSpec for PoolLive {
  // ...
  fn on_shutdown(pool: &Pool) { pool.close_blocking() }
}

// 0.7: an async finalizer effect, tied to the layer's scope
let pool: Layer<caps_next!(Pool), DbError> = Layer::acquire_release(
  || connect(),                      // runs uninterruptibly
  |env, _kind| close(env.0),         // Effect<(), Never, HNil>, may await
);
program.provide_layer(pool)          // released after `program`, however it ends
```

- `Layer::scoped(|scope| ..)` hands the build's `Scope` to the construction, which can call
  `scope.add_finalizer(..)` / `add_finalizer_exit(..)`. `Layer::acquire_release(acquire,
  release)` covers the usual case. `release(out, kind)` receives the closing `ExitKind`.
- **`Effect::provide_layer` now owns the layer's scope.** It closes after the effect completes
  (success, failure, defect or interrupt), and also when the build itself fails or is
  interrupted. This matches Effect.ts `Effect.provide(layer)`.
- `Layer::build_scoped(&scope)` builds into the caller's scope (Effect.ts
  `Layer.buildWithScope`). `Layer::launch()` builds and then waits until it is interrupted,
  which closes the scope (Effect.ts `Layer.launch`).
- `Layer::build()` has no owner for its scope. A failed or interrupted build releases what it
  had acquired, but **a successful `build()` never runs its finalizers**. Use one of the owners
  above for layers that hold resources.
- `layer.fresh()` builds its graph under a new memo table (Effect.ts `Layer.fresh`). It is
  built again even where the same layer is already part of the build. Its finalizers go to the
  same scope.
- `#[derive(ProviderSpec)]` also emits `Struct::layer::<E>()`, a
  `next::Layer<HCons<Service, HNil>, E>` that uses the same constructor. The v1 `ProviderSpec`
  impl is unchanged. Add `release = <fn(Arc<Service>) -> next::Effect<(), Never, HNil>>` to
  `#[provides(..)]` to give the layer an async finalizer. On the next path this replaces
  `on_shutdown`. A `with = |deps| ..` constructor reads a v1 `Env`, so it gets no `layer()`.
  The derive adds an inherent `layer` function, so a type that already defines its own `layer`
  must rename one of them.
