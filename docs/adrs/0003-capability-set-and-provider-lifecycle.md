# ADR 0003 — CapabilitySet, provider lifecycle, and advanced DI

## Status

Accepted

## Context

ADR 0002 shipped v2 runtime DI (`Env`, `ProviderSpec`, `run_with`) but left `caps!` as a stub expanding to `Env`, and `CapabilitySet::verify` as a no-op. Authors still pass explicit `env` to `require!` and platform keys remain public.

## Decision

### CapabilitySet encoding

Use **wrapper types** `CapEnvN<K1, …, Kn>` as the `Effect` `R` parameter:

- `caps!()` → `Env` (no required-cap verification beyond provider graph)
- `caps!(K1, K2, …)` → `CapEnvN<…>` wrapping built `Env`, implementing `Deref<Target=Env>`
- `CapabilitySet::verify(&Env)` checks all required keys present before `run_with` executes the app effect

Const-generic cap arrays deferred; wrapper types falsify quickly and integrate with existing `Effect::new(|env| …)`.

### Named variants

`CapabilityId` gains optional `variant: Option<&'static str>`. Providers declare `fn variant() -> Option<&'static str>`. Graph allows duplicate `TypeId` when variants differ.

### Non-object-safe traits

`Env::insert_any` / `get_any` store `Arc<dyn Any>` for generic cells; compile_fail tests document the exception path.

### Effectful providers

Optional `ProviderSpec::provide_effect` returns `Effect<T, E, Env>`. `run_with` runs effectful providers in topo order before the app effect.

### Scoped env and lifecycle

`Env::scoped(providers)` builds a child env layered on a parent clone. Providers may implement `on_shutdown` invoked reverse-topo after `run_with` completes.

## Consequences

- Semver 2.1.0 for `id_effect`; `id_effect_platform` 3.0.0 when hiding `HttpClientKey`.
- `EffectInterface` in `algebra/interface.rs` removed in favor of `Needs<K>` + `ProviderSpec`.
