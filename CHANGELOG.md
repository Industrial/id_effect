# Changelog

## Unreleased — Cap service names

### Changed

- Capability API uses **service type names** directly: `caps!(Counter)`, `require!(Counter)`, `#[provides(Counter)]`.
- `Cap<T>` replaces generated `*Key` types; `#[capability]` is a no-op (removed from public flow).
- Trait-backed services use type aliases (`HttpClientService`, `ConfigProviderService`, …).

### Removed

- Public `*Key` types and `define_capability!`.

## 0.4.0 — Implicit parallelism (breaking)

Semver-major release collapsing caller-facing parallelism policy into **Compute Fabric**. See [ADR 0008](docs/adrs/0008-implicit-parallelism.md) and [book ch13-05](crates/id_effect/book/src/part4/ch13-05-parallelism.md).

### Breaking

- **Removed public `Parallelism`** and all `*_with(policy, …)` dispatch on collections, `vec`, `order`, and `Stream`
- **Removed deprecated `*_par`**, `Stream::map_par_n`, `Stream::map_par_adaptive`, and public `compute::effective_threshold`
- Bulk and stream-chunk ops no longer accept caller policy — Fabric's `should_parallelize_current` is the sole threshold

### Added

- `compute::dispatch::parallel_if_profitable` — central Fabric-aware Rayon dispatch
- `Stream::map_effect` — admission-bounded concurrent effect mapping per chunk
- EDG parallel codegen for independent bind sets (`join_binds2` / `join_binds3` / `join_binds4`)
- Default Compute Fabric installation in `run()` and `run_with()`; `ensure_run_context()` at run boundaries
- Book rewrite: [Implicit Parallelism](crates/id_effect/book/src/part4/ch13-05-parallelism.md); Compute Fabric cross-refs in [ch12](crates/id_effect/book/src/part3/ch12-00-compute-fabric.md)
- Example: `122_compute_fabric_effect_parallel.rs` (`Stream::map_effect` under Fabric)

### Changed

- Primary `map`, `filter`, `map_values`, `sort_with`, and related bulk APIs parallelize implicitly when Fabric says it is profitable
- `effect!` may run independent `~` binds concurrently when the EDG finds no data or capability conflict
- `*_serial` and `#[effect(serial)]` remain the explicit escape hatches for `FnMut`, non-`Send`, and ordering

### Migration (from 0.3.x parallel-by-default)

| Old | New |
|-----|-----|
| `Parallelism::…` | removed — trust Fabric |
| `map_with(Parallelism::Serial, f)` | `map_serial(f)` |
| `map_with(Parallelism::ForceParallel, f)` | `map(f)` |
| `*_par(f)` | `map(f)` |
| `Stream::map_par_n(n, f)` | `map_effect(f)` |
| `Stream::map_par_adaptive(f)` | `map_effect(f)` |

Prior [ADR 0006](docs/adrs/0006-parallel-by-default.md) bulk behavior is retained internally; the public policy surface is superseded by [ADR 0008](docs/adrs/0008-implicit-parallelism.md).

### Version alignment

| Crate | Version |
|-------|---------|
| `id_effect` | **0.4.0** |
| workspace adapters | **0.4.0** |

## 0.3.0 — DI maturity (breaking)

Semver-major release completing capability-first DI adoption. See [appendix-b-migration.md](crates/id_effect/book/src/appendix-b-migration.md) and [ADR 0004](docs/adrs/0004-provider-parity-and-cap-subtyping.md).

### Removed

- `CapEnv1…CapEnv6` — use `CapList` / `caps!(K0, K1, …)` only
- `#[capability]` / `*Key` types — use service names with `Cap<T>`
- `require!(env, K)` — use `require!(K)` inside `effect!` or `Needs::<K>::need(env)`
- `ctx!`, `req!`, `service_key!`, `Layer`, `Stack`, `Effect::provide`, `IntoBind` (legacy paths)
- `id_effect_config::ambient` — use `Env::scoped` / `build_env`

### Added

- `CapList` + unbounded `caps!` arity
- `Cap<T>`, `#[derive(ProviderSpecDerive)]`, `#[provides(Service)]`, `#[named("variant")]`
- `CapWiden` for capability-set subtyping
- `ProviderSpec::optional_requires`, `shared()`, `refresh_interval()`, `on_refresh()`
- Fiber/request scoped overrides: `with_override`, `with_fiber_and_override`
- `id-effect-diagnose manifest` with TOML/JSON + `--json`
- Examples: `042_effectful_config_provider`, `043_named_variant_providers`, `id_effect_axum` `020_capability_run_with`
- Expanded trybuild corpus (12 cases) under `tests/ui/`

### Version alignment

| Crate | Version |
|-------|---------|
| `id_effect` | **0.3.0** |
| `id_effect_platform` | **4.0.0** |
| workspace adapters | **0.3.0** |

## 2.0.0 — Capability-first DI (breaking)

See prior entry for v2 initial capability DI release.
