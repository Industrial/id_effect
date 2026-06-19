---
name: id_effect
description: >-
  Router for id_effect Rust skills — Effect<A,E,R>, effect! macro, capability DI 3.0
  (Env, ProviderSpec, caps!, require!, run_with). Use when editing id_effect crates,
  examples, book, or workspace integration. Loads sub-skills for depth.
---

# id_effect (Rust) — Router

Quick reference for **id_effect 3.0**. For depth, read the specialized skill for your task — see [README.md](README.md).

## Pick a skill

| Task | Skill |
|------|-------|
| Learn Effect / `effect!` / `~` | [id_effect-fundamentals](id_effect-fundamentals/SKILL.md) |
| DI, providers, `caps!` | [id_effect-capabilities](id_effect-capabilities/SKILL.md) |
| Exit, Cause, recovery | [id_effect-errors](id_effect-errors/SKILL.md) |
| Fibers, scopes, Schedule | [id_effect-concurrency](id_effect-concurrency/SKILL.md) |
| Stream, Sink, Parallelism | [id_effect-streams](id_effect-streams/SKILL.md) |
| Schema / Unknown parsing | [id_effect-schema](id_effect-schema/SKILL.md) |
| Optics (Lens, Prism) | [id_effect-optics](id_effect-optics/SKILL.md) |
| Parser combinators / Codec | [id_effect-parse](id_effect-parse/SKILL.md) |
| Resilience / batching / breakers | [id_effect-resilience](id_effect-resilience/SKILL.md) |
| `run_test`, mocks | [id_effect-testing](id_effect-testing/SKILL.md) |
| Tokio, Axum, CLI | [id_effect-integration](id_effect-integration/SKILL.md) |
| FSM, saga, session types | [id_effect-fsm](id_effect-fsm/SKILL.md) |
| Event sourcing, projections, DAG | [id_effect-events](id_effect-events/SKILL.md) |
| Algebra / Foldable | [id_effect-algebra](id_effect-algebra/SKILL.md) |
| Optics | [id_effect-optics](id_effect-optics/SKILL.md) |
| FSM / saga | [id_effect-fsm](id_effect-fsm/SKILL.md) |
| Parse / codec | [id_effect-parse](id_effect-parse/SKILL.md) |
| Resilience | [id_effect-resilience](id_effect-resilience/SKILL.md) |
| Events / CQRS | [id_effect-events](id_effect-events/SKILL.md) |
| PR review | [id_effect-review](id_effect-review/SKILL.md) |

## Effect core (cheat sheet)

- **`Effect<A, E, R>`** — lazy program; run with `run_blocking`, `run_async`, or `run_with`.
- **`effect!`** — do-notation inside `|r| { … }`.
- **`~expr`** — bind an `Effect` step (sequencing).
- **`~Key`** — borrow a capability from `r` (`require!(Key)` alias).

## Capability DI (3.0)

| Do | Pattern |
|----|---------|
| Declare key | `#[::id_effect::capability(T)] struct Name;` → `NameKey` |
| Declare provider | `#[derive(ProviderSpecDerive)]` + `#[provides(NameKey)]` |
| Typed requirements | `Effect<_, _, caps!(K1, K2)>` |
| Access in `effect!` | `~NameKey` with `|r|` |
| Wire at edge | `run_with([provide!(Live), …], effect)` |
| Test doubles | `mock_capability!` or `env.insert::<Key>(value)` |

## Removed (do not use)

- `define_capability!`, `service_key!`, `ctx!`, `req!`
- `Layer` / `Stack`, `Effect::provide`, `CapEnv1…6`
- `require!(env, Key)`, config `ambient`
- Service `IntoBind` (`~ServiceKey`) for DI — use `~Key`

## Parallelism (Rayon default)

- **`Parallelism::Auto { threshold: 1024 }`** — default for bulk collection/stream chunk ops
- **`effect!` stays sequential** — use `fiber_all` or `Stream::map_par_n` for concurrency
- Escape: `*_serial` for `FnMut` / non-`Send`; `*_with(Parallelism::…, …)` for explicit policy
- ADR: `docs/adrs/0006-parallel-by-default.md`

## Verify

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo test -p id_effect --test ui_compile_fail
cd crates/id_effect/book && mdbook build
```

## Docs

- Book: `crates/id_effect/book/`
- Migration: `book/src/appendix-b-migration.md`
- ADRs: `docs/adrs/0002-*` … `0006-*`

Say **id_effect capability DI** — not "v2 DI" or Layer terminology in outward docs.
