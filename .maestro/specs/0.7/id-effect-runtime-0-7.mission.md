---
title: id_effect 0.7.0 — the runtime release (fiber runtime, Send, typed R, Effect.ts parity)
slug: id-effect-runtime-0-7
mode: heavy
work_type: initiative
risk_class: high
version: 2
acceptance_criteria:
  - A draft PR release/0.7.0 to main exists from W0 so that ci-pass and pg-integration witness every leaf, and the W0 CI run records the baseline coverage percentage
  - Criterion baselines of v0.6.0 are committed as docs/benchmarks/v0.6.json with machine info and the bench/v0.6 SHA, covering bind/map chains, async bind, flat_map depth, stream pull/map_effect, channel/queue, fiber spawn/join, fiber sleep wall and CPU time, and the axum request path, and scripts/bench-compare.sh enforces the D9 thresholds from docs/benchmarks/gates.toml
  - id_effect_tower and id_effect_sql_pg are deleted, and so are StreamV1, ComputeSupervisor, SchemaParser, the #[capability] no-op, the platform::process alias, the root re-exports pipe1/2/3, memoize, flip, const_, tupled, untupled and MutableRef, and the lib.rs back-compat module aliases; labs call sites are updated in the same commits; rg finds none of them in crates or labs sources; id_effect_fp and id_effect_optics are kept
  - The request resolver awaits its batch fetch instead of calling run_blocking(..).expect, a fetch error fails every waiting entry with a typed error, and a current_thread tokio test with an async fetch completes
  - Cause::Die carries a Defect that keeps the panic payload, and a panic in any fiber, finalizer or runtime edge surfaces as Cause::Die with a downcastable payload
  - Both kernel designs are spiked in parallel — A in spikes/kernel_a and B as the standalone spikes/kernel_b crate — sharing the inline Ready variant, Cause<E> as internal error channel, a one-shot completion slot and effect_local!/LocalEffect, and docs/benchmarks/SPIKE-REPORT.md compares them side by side against v0.6 (bench ratios, at least three trybuild .stderr compile errors each, inference annotation counts on a shared corpus, cargo check time at 16 caps by 100 handlers, B's showcase env forward-list size) before the human design choice H1
  - ADR 0011 (fiber runtime, Send by default, typed R, side-by-side transition) is accepted, and ADRs 0003, 0007 Phase C, 0008 and 0010 are amended to match the code; ADR 0010 records that tokio stays mandatory
  - The chosen kernel lands side by side in id_effect::next with the old kernel untouched, and every leaf from W1 on keeps cargo check --workspace, scripts/ci-local.sh full and the labs workspace tests green
  - Effect<A,E,R> is Send by default with an opt-in LocalEffect, fibers run on an Executor trait with mandatory tokio as the default backend and real wakers, interruption stops work and runs finalizers, parent fibers interrupt their children on exit, and a missing capability is a compile error
  - race, race_all, timeout, zip_par, all and for_each with Sequential, Bounded, Unbounded and Inherit concurrency, fork, fork_daemon, interruptible and uninterruptible exist, join_binds fails fast with sibling interruption, and coordination and resource constructors are plain functions
  - Layer values support merge, provide_to, fresh and scoped with async finalizers, Schedule<Out,In> is an immutable typed value with retry, retry_or_else, repeat and repeat_or_else Effect methods, Effect::request batches and caches, and Random and TestRandom exist
  - Metrics use atomics and bounded bucketed histograms behind a registry, every Schema combinator round-trips decode(encode(a)) == a with struct arity of at least 12, and Config<T> is the sole configuration API and is providable as a Layer
  - All 17 published crates switch to the new kernel in publish-level order, with unmigrated dependents bridged through id_effect::next::compat until they switch; axum handlers are native Send without block_in_place or run_local, resilience hedged uses race, STM retry no longer spins, SpinClock is deleted, and the logger keeps its thread-local install API
  - All labs crates (parse, events, graph, workflow, jobs, rpc, ai, plus lint message text) are migrated to 0.7 with no new features and the labs CI job green
  - k-retire-v1 deletes the old kernel and the compat bridge only after no next::compat call site remains, and k-retire-sweep leaves no id_effect::next or temporary *_next! macro reference in crates, labs or examples; no library target calls run_blocking except allowlisted edges
  - examples/http-showcase (axum + sql pg + config + otel) uses Layers, Scope, typed errors, retry with a Schedule and per-request timeout, and its integration tests against a real Postgres are green in the CI pg-integration job, failing rather than skipping when ID_EFFECT_REQUIRE_PG=1 and no database is reachable
  - docs/benchmarks/REPORT-0.7.md compares a same-session re-run of bench/v0.6 with 0.7.0, showing bind/map regress by at most 25 percent while fiber sleep CPU, fiber spawn and the axum request path improve, or records an explicit human waiver per D9
  - docs/MIGRATION-0.7.md covers every breaking change and removal with its replacement, the CHANGELOG has a 0.7.0 section, and the book, READMEs and Effect.ts parity checklist match the code
  - The coverage floor in scripts/llvm-cov-nextest.sh and docs/QUALITY_BAR.md is 96 percent and is met; moon CI, tier conformance, docs with -D warnings, cargo audit, MSRV 1.94, minimal-versions, crate packaging, labs and pg-integration are green on the release PR; all workspace and labs crates are versioned 0.7.0
non_goals:
  - New features in labs crates (migration only)
  - Making tokio optional, or replacing the logger's thread-local install API
  - New published crates (only examples/http-showcase and a temporary spikes/ directory are added)
  - Effect.ts parity beyond decision D7 (no Stream operator sweep, no STM redesign beyond removing the spin, no Cache structural sharing or LRU rework)
  - Fixing the effect! token-level >> limitation unless the kernel spec absorbs it
  - Observability unification on tracing, and downstream (assay, solana) migration — follow-up mission after publish
  - Running benchmarks in CI
  - Git worktrees — all work happens in the main checkout on release/0.7.0
---

# id_effect 0.7.0 — the runtime release

This release replaces the non-Send, busy-polled kernel with an owned fiber interpreter on an `Executor`
trait, with tokio as the mandatory default backend. `Effect` becomes Send by default, R becomes typed
so that a missing capability is a compile error, and the Compute Fabric becomes the scheduler behind
parallel combinators. On top of that the release adds the D7 Effect.ts parity layer and the D8 prune.

**The transition is side by side.** The new kernel lands as `id_effect::next`. Crates and labs switch
to it in dependency order, so the workspace stays green at every commit. The old kernel is then
retired. Everything ships as one breaking 0.7.0.

**Where work happens:** in the main checkout `/data/Code/rust/id_effect` on branch `release/0.7.0`.
There are no worktrees.

**Release gate (D10):**
- a green showcase HTTP service;
- a bench report against the v0.6 baseline;
- a migration guide;
- the existing gates, with coverage at 96 %.

**Decisions:**
- Approved decisions: D1–D10 (2026-10-14) in `.maestro/specs/0.7/brief.md`.
- Human rulings: R1–R7 (2026-10-15) in `.maestro/specs/0.7/release-plan.md` §9.
- The kernel internals are specified in `.maestro/specs/0.7/kernel-design-a-hlist.md` and
  `.maestro/specs/0.7/kernel-design-b-traitenv.md`. Both are spiked, and the human picks one at H1.
  The prior is A.

**Where the details are.** `.maestro/specs/0.7/release-plan.md` has:
- the waves, with contract paths, acceptance criteria and gates for each leaf;
- the K boundary;
- the side-by-side site and the interop rules (§3a, §3b);
- the hot-file register;
- the risks.

**Human gates:**
- **H0:** plan approval.
- **H1:** kernel design.
- **H2:** D9 waiver, if needed.
- **H3:** tag and publish to crates.io.

## Decompose batch (for maestro_mission_decompose)

Decompose does not set `blocked_by`. The edges are in release-plan §2 and §4. Copy them into
`.maestro/missions/id-effect-runtime-0-7.execution.md` when the mission is materialized.

The batch has **71 leaves**:

| Wave | Slugs |
|---|---|
| 0 | w0-draft-pr, b0-bench-core, b0-bench-fiber, b0-bench-axum, b0-bench-tooling, b0-baseline-capture |
| 1 | w1-drop-shim-crates, w1-request-resolver, w1-defect-type, w1-prune-root-exports, w1-prune-deprecated, w1-prune-macros |
| 2 | w2-spike-harness, k-spike-a, k-spike-b, w2-spike-gate, w2-adr-0011, w2-adr-0003-amend, w2-adr-0007-amend, w2-adr-0008-amend, w2-adr-0010-amend |
| 3 | k-rebuild, w3-scaffold, c-compat-bridge, c-race-timeout, c-zip-par-all, c-join-binds, c-fabric-scheduler, c-coordination, c-resource, c-panic-boundary |
| 4 | p-layer-core, p-layer-scoped, p-schedule, p-metrics, p-request-batch, p-request-cache, p-random, p-schema-complete, p-config-unify |
| 5 | m-tokio, m-streams, m-cli, m-optics, m-fp, m-logger, m-config, m-platform, m-process, m-resilience, m-sql, m-fsm, m-node, m-otel, m-axum, l-labs-core, l-labs-durable, l-labs-edge, k-retire-v1, k-retire-sweep |
| 6 | s-scaffold, s-domain, s-otel, s-it |
| 7 | r-coverage-floor, r-bench-report, r-migration-guide, r-docs-sweep, r-quality-gates, r-version-bump, r-release |

`k-spike-a`, `k-spike-b` and `k-rebuild` are placeholders. `maestro_task_split` expands each one into
the leaves of its kernel spec: the spikes at W2 start, the rebuild after H1. `k-retire-v1` may split in two.
