# capability — v2 dependency injection

Trait-first DI: [`Env`](env.rs), [`ProviderSpec`](provider.rs), [`CapabilityGraph`](graph.rs), [`run_with`](run.rs).

## Author flow

1. `define_capability!(MyKey, MyService)`
2. `impl ProviderSpec for MyLive { ... }`
3. `run_with([provide!(MyLive)], app())`

See `examples/040_capability_app.rs` and ADR 0002.
