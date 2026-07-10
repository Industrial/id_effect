# Composing Providers — `CapabilityGraph` and `run_with`

Individual providers build one capability. Applications pass a **list** to [`run_with`](../../src/capability/run.rs); [`CapabilityGraph`](../../src/capability/graph.rs) plans build order from each provider's `requires()` and `provides()`.

## Basic composition

```rust
use id_effect::{provide, run_with};

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

The array order is irrelevant — the graph topologically sorts providers. Cycles or missing dependencies surface as [`CapabilityPlannerError`](../../src/capability/error.rs) with diagnostics from [`CapabilityGraph::diagnostics`](../../src/capability/graph.rs).

## Building `Env` without running

```rust
let env = build_env([
    provide!(ConfigLive),
    provide!(DatabaseLive),
    provide!(UserRepoLive),
])?;

run_test(get_user(1), env)?;
```

Or inspect the plan explicitly:

```rust
let mut graph = CapabilityGraph::new();
graph = graph.add(provide!(ConfigLive).0);
graph = graph.add(provide!(DatabaseLive).0);
let order = graph.plan()?;
let env = graph.build()?;
```

## Production vs test stacks

```rust
// Production
run_with(
    [provide!(ConfigLive), provide!(DatabaseLive), provide!(UserRepoLive)],
    my_app(),
)?;

// Test — swap implementations, same keys
run_with([provide!(MockUserRepoLive)], get_user(1))?;
```

Application code is unchanged. Only the provider list differs. `Needs<K>` bounds ensure each effect's requirements are met.

## Subset wiring in tests

Tests need not mirror the full production graph — only the capabilities the effect under test requires:

```rust
#[test]
fn test_get_user() {
    let env = build_env([provide!(MockUserRepoLive)]).unwrap();
    let user = run_test(get_user(1), env).unwrap();
    assert_eq!(user.name, "Alice");
}
```

If `get_user` also needs `EffectLogger`, the test must include `provide!(TestLoggerLive)` or insert that key manually — incomplete wiring fails when the effect runs.
