# ADR 0002 — Capability-first DI (v2)

## Status

Accepted. Superseded in part (2026-10-15) by [ADR 0011](0011-fiber-runtime-send-typed-r.md):
in 0.7 `R` is a typed HList checked at compile time, `Env` survives only as the fallible
`DynEnv`, and providers become `Layer` values. See
[ADR 0003 Amendment 1](0003-capability-set-and-provider-lifecycle.md#amendment-1-2026-10-15-for-07).

## Context

id_effect v1 exposes Effect.ts-style dependency injection via `Tag`, `service_key!`, HList `Cons`/`Nil`, and manual `Layer`/`Stack` composition. This is type-safe but verbose, leaks HList structure into error messages, and requires manual wiring at app entrypoints.

## Decision

Adopt a **trait-first capability model** with:

1. **`Cap<T>`** — universal capability slot; `T` is the service type used in `caps!(T)`.
2. **`caps!(Database)`** — expands to `CapList<(Cap<Database>,)>`.
3. **`Env`** — order-independent runtime container.
4. **`Provider<P>`** — each live/test impl provides itself, optionally reading deps from `Env`.
5. **`CapabilityGraph`** — topological provider resolution (extracted from `LayerGraph` planner).
6. **`run` / `run_with`** — single app entrypoint.
7. **Clean break v2.0** — remove v1 public DI symbols.

## Object safety

Default path: object-safe capability traits stored as concrete types in `Env` via `Env::insert<P: ProviderValue>()`.

Non-object-safe traits: use generic env cells (`Env::insert_generic<T>()`) — documented exception; compile_fail test required.

## v1 → v2 mapping

| v1 (removed) | v2 (public) |
|--------------|-------------|
| `service_key!` + `Tag<K>` | `caps!(Trait)` |
| `Service<K,V>` / `Tagged<K,V>` | `Env::get::<Cap<T>>()` |
| `req!(K: V \| …)` | `caps!(Trait, …)` |
| `ctx!(K => v)` | `Env::from_providers([…])` |
| `Layer` / `Stack` / `layer_service` | `Provider<P>` + `CapabilityGraph` |
| `Effect::provide(ctx)` | `run_with(providers, effect)` |
| `NeedsHttpClient` + `Get` | `Needs<Trait>` |
| `~EffectLogger` + `IntoBind` | `require!(Trait)` in `effect!` |

## Consequences

- Semver-major release; all workspace crates migrate in same release.
- `EffectInterface` in `algebra/interface.rs` superseded by `Needs<Trait>`.
- Book part 2 chapters rewritten.
