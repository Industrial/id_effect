# ADR 0004 — Provider parity, cap subtyping, and bundle validation

## Status

Accepted

## Context

ADR 0003 shipped `CapEnv1…6`, sync providers, and runtime `CapabilitySet::verify`. Effect.ts Layer semantics include optional dependencies, shared memoized singletons, refresh hooks, and capability-set subtyping. The maturity mission requires a clean break to `CapList`, mandatory compile-time caps, and production-grade provider graph behavior.

## Decision

### CapList encoding

Replace fixed `CapEnvN` wrappers with a single generic:

```rust
pub struct CapList<Ks>(Env, PhantomData<Ks>);
```

`Ks` is a tuple of `Capability` types `(K0, K1, …)`. The `cap_keys!` macro generates `CapKeys` impls for arities 0–16. `caps!(K0, K1, …)` expands to `CapList<(K0, K1, …)>`. `caps!()` remains `Env`.

### Capability-set subtyping (`CapWiden`)

A provider effect requiring `caps!(Db)` is assignable where `caps!(Db, Log)` is expected: the wider runtime env satisfies the narrower requirement. Implement via:

```rust
pub trait CapWiden<Target> {
  fn widen(self) -> Target;
}
```

Blanket: `CapList<(A, B, …)>` widens to `CapList<(A,)>` when `A` is the first tuple element and remaining keys are present in env (structural subset check at compile time via trait impls per arity).

### Optional provider dependencies

`ProviderSpec::requires` may include keys marked optional via `ProviderSpec::optional_requires() -> &'static [CapabilityId]`. `CapabilityGraph::plan` skips missing optional deps instead of failing.

### Shared memoized providers

`ProviderSpec::shared() -> bool` default `false`. When `true`, `CapabilityGraph` stores output in `Arc<OnceLock<…>>` keyed by `CapabilityId` so multiple dependents share one instance.

### Refreshable providers

`ProviderSpec::refresh_interval() -> Option<Duration>` and `on_refresh(&mut self)` hook. Graph registers refresh handles; `run_with` spawns refresh task when interval set.

### Compile-time bundle validation

`providers!(dev: […])` static array validated at compile time where possible (duplicate `CapabilityId` → compile error via const eval in future). Runtime: `CapabilityGraph::diagnostics()` reports conflicts before `build`.

## Consequences

- `CapEnv1…6` deleted; migration: `CapEnv3<A,B,C>` → `CapList<(A,B,C)>`.
- Semver **0.3.0** for `id_effect`; **4.0.0** for `id_effect_platform`.
- Optional/refresh/shared semantics documented for cookbook examples in waves 4–5.
