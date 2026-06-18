# Changelog

## 3.0.0 — DI maturity (breaking)

Semver-major release completing capability-first DI adoption. See [appendix-b-migration.md](crates/id_effect/book/src/appendix-b-migration.md) and [ADR 0004](docs/adrs/0004-provider-parity-and-cap-subtyping.md).

### Removed

- `CapEnv1…CapEnv6` — use `CapList` / `caps!(K0, K1, …)` only
- `define_capability!` — use `#[capability]` attribute
- `require!(env, K)` — use `require!(K)` inside `effect!` or `Needs::<K>::need(env)`
- `ctx!`, `req!`, `service_key!`, `Layer`, `Stack`, `Effect::provide`, `IntoBind` (legacy paths)
- `id_effect_config::ambient` — use `Env::scoped` / `build_env`

### Added

- `CapList` + unbounded `caps!` arity
- `#[capability]`, `#[derive(ProviderSpec)]`, `#[named("variant")]`
- `CapWiden` for capability-set subtyping
- `ProviderSpec::optional_requires`, `shared()`, `refresh_interval()`, `on_refresh()`
- Fiber/request scoped overrides: `with_override`, `with_fiber_and_override`
- `id-effect-diagnose manifest` with TOML/JSON + `--json`
- Examples: `042_effectful_config_provider`, `043_named_variant_providers`, `id_effect_axum` `020_capability_run_with`
- Expanded trybuild corpus (12 cases) under `tests/ui/`

### Version alignment

| Crate | Version |
|-------|---------|
| `id_effect` | **3.0.0** |
| `id_effect_platform` | **4.0.0** |
| workspace adapters | **3.0.0** |

## 2.0.0 — Capability-first DI (breaking)

See prior entry for v2 initial capability DI release.
