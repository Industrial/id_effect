---
title: 'T12 carry (F52) — Effect::any_env, run an env-free effect under any environment: r13-f-any-env'
slug: r13-f-any-env
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'F52 / D14: `impl<A, E> Effect<A, E, ()> { pub fn any_env<R>(self) -> Effect<A, E, R> }` in crates/id_effect/src/kernel/effect.rs beside `widen` (`:237`), and the same on `LocalEffect` in kernel/local.rs; no extra allocation beyond the existing representation; rustdoc example lifts an env-free effect into a procedure DynEnv effect (the 0.12 Ledger pattern `Effect::from_ctx(|ctx| async move { ctx.bind(e).await })` becomes one call).'
  - 'Tests `any_env_runs_under_unit_and_dyn_env`, `any_env_preserves_interruption_and_finalizers`, `local_any_env_round_trip`; one book section on effect environments (part2) compiled by book-snippets.'
  - 'Tier: core (strict bar: no new dependency, no unsafe, docs on every item). Secure by default: no new surface.'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect --summary-only --fail-under-regions 96`; new code 100 % regions (two small methods).'
  - 'Gate: `cargo nextest run -p id_effect`; `cargo test -p id_effect --doc any_env`; `cargo test -p book-snippets`'
  - 'Owns: `crates/id_effect/src/kernel/{effect.rs,local.rs}` (the `any_env` methods only), `crates/id_effect/tests/any_env.rs`, one section of the part2 effect-environment chapter under `crates/id_effect/book/src/part2/`'
---

# r13-f-any-env

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T12 carry" (0.12 overlay F52, prerequisite of
F27). The release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 0 head. Blocked by: the v0.12.0 tag. Blocks r13-f-client-batching.
