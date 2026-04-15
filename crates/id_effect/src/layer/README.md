# `layer` — Stratum 5: layers & dependency injection

**Composable construction** of the environment `R`: [`Layer`](factory.rs), [`Stack`](factory.rs) / [`StackThen`](factory.rs), [`Service`](service.rs) / [`ServiceEnv`](service.rs), and [`LayerGraph`](graph.rs) for name-based dependency planning.

## What lives here

| Module | Role |
|--------|------|
| `factory` | `Layer`, `LayerFn`, `Stack`, `merge_all`, `LayerEffect`, … — recipes `≅ Effect[Out, Err, ()]` for one cell. |
| `service` | `Service`, `service`, `service_env`, `provide_service`, `layer_service` — Effect.ts-style DI. |
| `graph` | `LayerGraph`, `LayerNode`, `LayerPlan` — topological build from requires/provides. |

## What it is used for

- **Building** `Context<Cons<…>>` values by stacking layers (each layer may run effects to produce a tagged value).
- **Wiring** implementations to interfaces via tags and `provide_service`.
- **Large graphs** — use `LayerGraph` when declarative edges beat manual `Stack` order.

## Best practices

1. **Think `Layer[Out,Err] ≅ Effect[Out,Err,()]`** — layers are effectful constructors of one piece of `R`.
2. **Resolve cycles** at plan time — `LayerGraph` reports `LayerPlannerError` with diagnostics.
3. **Expose services** through tags and `Needs*` traits in downstream crates, not raw global singletons.
4. **Keep layers pure of application policy** where possible — business rules belong in `Effect` graphs that **use** the environment.

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 5.
- [`context`](../context/README.md) — structure of `R`.
- [`runtime`](../runtime/README.md) — `run_blocking` to execute layer effects when building the stack.
