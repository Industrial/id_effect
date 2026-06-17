# Changelog

## 2.0.0 — Capability-first DI (breaking)

Semver-major release removing v1 Effect.ts-style dependency injection in favor of trait-first capability DI. See [ADR 0002](docs/adrs/0002-capability-di-v2.md).

### Removed (public API)

- HList context types: `Cons`, `Nil`, `Tag`, `Tagged`, `Context`, `Get`, `GetMut`, `Here`, `Skip*`, `There*`
- Layer stack: `Layer`, `Stack`, `LayerFn`, `LayerGraph`, `layer_service`, `provide_service`, `service`, `Service`, `ServiceEnv`
- Macros: `ctx!`, `req!`, `service_key!`, `service_def!`, `layer_graph!`, `layer_node!`
- Public `context` and `layer` modules (now `pub(crate)`; HList engine remains internal)

### Added / retained (v2 public API)

- `define_capability!`, `caps!`, `provide!`, `require!`
- `Env`, `ProviderSpec`, `ProviderBox`, `CapabilityGraph`, `Needs`, `run`, `run_with`, `build_env`
- `Matcher` / `HasTag` (pattern matching; not part of DI)

### v1 → v2 migration

| v1 (removed) | v2 (public) |
|--------------|-------------|
| `service_key!` + `Tag<K>` | `define_capability!(Trait)` |
| `Service<K,V>` / `Tagged<K,V>` | `Env::get::<K>()` / `require!(env, K)` |
| `req!(K: V \| …)` | `caps!(Trait, …)` |
| `ctx!(K => v)` | `build_env([provide!(Live), …])` or `Env::insert` |
| `Layer` / `Stack` / `layer_service` | `ProviderSpec` + `CapabilityGraph` |
| `Effect::provide` / `provide_service` | `run_with(providers, effect)` |
| `NeedsHttpClient` + `Get` | `Needs<Trait>` |
| `~EffectLogger` + `IntoBind` | `require!(Trait)` in `effect!` |

Workspace crates (`id_effect_platform`, `id_effect_config`, `id_effect_reqwest`, …) ship v2 providers in this release.
