---
name: id_effect
description: >-
  Write and review id_effect Rust code — Effect<A,E,R>, effect! macro, capability
  DI (Env, ProviderSpec, caps!, require!, run_with). Use when editing id_effect
  crates, examples, book, or workspace integration (platform, config, logger,
  axum, tokio).
---

# id_effect (Rust)

Authoritative patterns for **id_effect 3.0** capability DI. Do not use removed APIs.

## Effect core

- **`Effect<A, E, R>`** — lazy program; run with `run_blocking`, `run_async`, or `run_with`.
- **`effect!`** — do-notation inside `|r| { ... }` (or explicit `|r: &mut caps!(…)|`).
- **`~expr`** — bind an `Effect` step (sequencing). Still required for running effects.
- **`~Key`** — borrow a capability from `r` inside `effect!` (`require!(Key)` is an alias).

## Capability DI (3.0)

| Do | Pattern |
|----|---------|
| Declare key | `#[::id_effect::capability(T)] struct Name;` → `NameKey` |
| Declare provider | `#[derive(::id_effect::ProviderSpecDerive)]` + `#[provides(NameKey)]` |
| Typed requirements | `Effect<_, _, caps!(K1, K2)>` (not bare `Env` on public multi-cap APIs) |
| Access in `effect!` | `~NameKey` (or `require!(NameKey)`) with `|r|` or explicit `caps!(…)` |
| Access outside | `Needs::<NameKey>::need(env)` |
| Wire at edge | `run_with([provide!(Live), ...], effect)` or `build_env([...])` |
| Test doubles | `mock_capability!` or `env.insert::<Key>(value)` |

## Removed (do not use or document as current)

- `define_capability!`, `service_key!`, `ctx!`, `req!`
- `Layer` / `Stack`, `Effect::provide`, `CapEnv1…6`
- `require!(env, Key)`, config `ambient`
- Service `IntoBind` (`~ServiceKey`) — use `~Key` for DI lookup

## Kernel `IntoBind` (still valid)

`Effect` and `Result` implement `IntoBind` for **`~`** in `effect!` — e.g. `~Ok(42)`, `~logger.info(...)`. This is not DI lookup.

## Workspace crates

| Crate | Role |
|-------|------|
| `id_effect` | Core |
| `id_effect_platform` | FS, HTTP, process capabilities |
| `id_effect_config` | Figment/config providers |
| `id_effect_logger` | `EffectLogger` capability |
| `id_effect_tokio` | `run_async` on Tokio |
| `id_effect_axum` | `run_with_caps`, routing helpers |
| `id_effect_reqwest` | reqwest client capability |

## Docs & migration

- Book: `crates/id_effect/book/`
- Migration: `book/src/appendix-b-migration.md` (1.x → 3.0)
- ADRs: `docs/adrs/0002-*`, `0003-*`, `0004-*`, `0005-*`

## Verify

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo test -p id_effect --test ui_compile_fail
cd crates/id_effect/book && mdbook build
```

## Outward communication

Say **id_effect capability DI** or **id_effect** — not "v2 DI". Semver labels (1.x, 3.0) only in migration/history sections.

## Book DI conventions

When writing book examples:

- `Effect<_, _, caps!(Key, …)>` — never bare `Database` / `(Database, Logger)` as `R`
- `effect!(|r| { ~Key; ~other_effect(); … })` — not `Effect::new` + `Needs::need`
- `run_with([provide!(Live)], effect)` — not `.provide()`
- Key types end in `Key`: `DatabaseKey`, `LoggerKey`, `UserRepoKey`
