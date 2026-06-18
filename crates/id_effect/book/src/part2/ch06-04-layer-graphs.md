# Capability Graphs — Automatic Dependency Resolution

For small applications, passing a short provider list to [`run_with`](../../src/capability/run.rs) is enough. For larger apps, [`CapabilityGraph`](../../src/capability/graph.rs) plans build order from each provider's `requires()` / `provides()` metadata and surfaces diagnostics via [`CapabilityGraph::diagnostics`](../../src/capability/graph.rs).

## Declaring a provider graph

```rust
use id_effect::{CapabilityGraph, provide};

let graph = CapabilityGraph::new()
    .add(provide!(ConfigLive).0)
    .add(provide!(DatabaseLive).0)
    .add(provide!(CacheLive).0);
```

Each `ProviderSpec` declares dependencies via `requires()`; `CapabilityGraph` topologically sorts providers before calling `build` / `build_from`.

## Planning and building

```rust
let order = graph.plan()?;
let env = graph.build()?;
```

The planner returns node indices in dependency order. Independent branches may appear in any stable topological order.

## Cycle detection

`graph.plan()` returns an error if there are circular dependencies:

```rust
let diags = bad_graph.diagnostics();
assert!(!diags.is_empty()); // e.g. cycle-detected
```

Cycles and missing providers are detected at plan time via [`CapabilityPlannerError::to_diagnostic`](../../src/capability/error.rs). Use `cargo run -p id_effect_cli --bin id-effect-diagnose -- example cycle` to print a sample report.

## Conditional providers

Layers can be added conditionally:

```rust
let mut providers = vec![provide!(ConfigLive)];
if cfg!(feature = "metrics") {
    providers.push(provide!(MetricsLive));
}
let env = build_env(providers)?;
```

Feature flags and environment-based configuration compose naturally with the graph API.

## When to use graphs vs hand-built `Env`

| Situation | Prefer |
|-----------|--------|
| < 5 capabilities, tests | `build_env([...])` |
| Complex `requires()` graphs | `CapabilityGraph` |
| Need diagnostics / cycles | `CapabilityGraph::diagnostics` |
| Request-local overrides | `Env::scoped` |
| CLI troubleshooting | `id-effect-diagnose` |
