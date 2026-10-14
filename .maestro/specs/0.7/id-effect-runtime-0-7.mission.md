---
title: id_effect 0.7.0 — the runtime release (fiber runtime, Send, typed R, Effect.ts parity)
slug: id-effect-runtime-0-7
mode: heavy
work_type: initiative
risk_class: high
version: 1
acceptance_criteria:
  - Criterion baselines of v0.6.0 for bind/map chains, async bind, flat_map depth, stream pull/map_effect, channel/queue, fiber spawn/join, fiber sleep wall and CPU time, and the axum request path are committed as docs/benchmarks/v0.6.json with machine info and the bench/v0.6 SHA, and scripts/bench-compare.sh enforces the D9 thresholds from docs/benchmarks/gates.toml
  - id_effect_tower and id_effect_sql_pg are deleted, StreamV1, ComputeSupervisor, SchemaParser, the #[capability] no-op, the platform::process alias, the pipe1/2/3, memoize, flip, const_, tupled, untupled and MutableRef root re-exports, and the lib.rs back-compat module aliases are removed, and rg finds none of them in crates/*/src; id_effect_fp and id_effect_optics are kept
  - The request resolver awaits its batch fetch instead of calling run_blocking(..).expect, a fetch error fails every waiting entry with a typed error, and a current_thread tokio test with an async fetch completes
  - Cause::Die carries a Defect that keeps the panic payload, and a panic in any fiber, finalizer or runtime edge surfaces as Cause::Die with a downcastable payload instead of aborting or losing the payload
  - A kernel spike for each design (kernel-design-a-hlist, kernel-design-b-traitenv) is measured against v0.6 in docs/benchmarks/SPIKE-REPORT.md, and the human design choice (H1) is recorded before any destructive kernel change
  - ADR 0011 (fiber runtime, Send by default, typed R) is accepted, and ADRs 0003, 0007 Phase C, 0008 and 0010 are amended to match the code, including map_effect sequential plus map_effect_par(n) and the Fabric as the admission scheduler behind fork_all
  - Effect<A,E,R> is Send by default with an opt-in LocalEffect, fibers run on an Executor trait with tokio as the default backend and real wakers, interruption stops work and runs finalizers, parent fibers interrupt their children on exit, and a missing capability is a compile error
  - race, race_all, timeout, zip_par, all and for_each with Sequential, Bounded, Unbounded and Inherit concurrency, fork, fork_daemon, interruptible and uninterruptible exist, join_binds fails fast with sibling interruption, and pure constructors such as Deferred, Semaphore, Cache and Bulkhead are plain functions
  - No library (non-test) target in the workspace calls run_blocking, except allowlisted binary and CLI edges, as checked by scripts/check-no-run-blocking.sh --all, and SpinClock and the process node workaround are deleted
  - Layer values support merge, provide_to, fresh and scoped with async finalizers, Schedule<Out,In> is an immutable typed value with retry, retry_or_else, repeat and repeat_or_else Effect methods and the free functions removed, Effect::request batches and caches, and Random and TestRandom exist
  - Metrics use atomics and bounded bucketed histograms behind a registry, every Schema combinator round-trips decode(encode(a)) == a with struct arity of at least 12, and Config<T> is the sole configuration API and is providable as a Layer
  - All 17 published crates are migrated to the new kernel; logger uses capability or FiberRef DI instead of thread-locals, axum handlers are native Send without block_in_place or run_local, resilience hedged uses race, and STM retry no longer spins
  - examples/http-showcase (axum + sql pg + config + otel) uses Layers, Scope, typed errors, retry with a Schedule and per-request timeout, and its integration tests against a real Postgres are green in the CI pg-integration job, failing rather than skipping when ID_EFFECT_REQUIRE_PG=1 and no database is reachable
  - docs/benchmarks/REPORT-0.7.md compares a same-session re-run of bench/v0.6 with 0.7.0, showing bind/map regress by at most 25 percent and fiber sleep CPU, fiber spawn and the axum request path improve, or records an explicit human waiver per D9
  - docs/MIGRATION-0.7.md covers every breaking change and removal with its replacement, the CHANGELOG has a 0.7.0 section, and the book, READMEs and Effect.ts parity checklist match the code
  - All gates are green on the release PR — moon CI, coverage at the agreed floor (D10 96 percent unless Q1 decides otherwise), tier conformance, docs with -D warnings, cargo audit, MSRV 1.94, minimal-versions, crate packaging and pg-integration — and all workspace crates are versioned 0.7.0 with labs still building against id_effect 0.6.0
non_goals:
  - Migrating or editing labs/ crates beyond pinning their id_effect dependencies to 0.6.0
  - New published crates (only examples/http-showcase and a temporary spikes/ member are added)
  - Effect.ts parity beyond decision D7 (no Stream operator sweep, no STM redesign beyond removing the spin, no Cache structural sharing or LRU rework)
  - Fixing the effect! token-level >> limitation unless the kernel spec absorbs it
  - Observability unification on tracing, and downstream (assay, solana) migration — follow-up mission after publish
  - Running benchmarks in CI
---

# id_effect 0.7.0 — the runtime release

This release replaces the non-Send, busy-polled kernel with an owned fiber interpreter on an `Executor`
trait, with tokio as the default backend. `Effect` becomes Send by default, R becomes typed so that a
missing capability is a compile error, and the Compute Fabric becomes the scheduler behind parallel
combinators. On top of that, the release adds the Effect.ts parity layer from D7, prunes the 0.6
deprecations from D8, and migrates all 17 crates. It ships as one breaking 0.7.0.

The release gate (D10) is:
- a green showcase HTTP service;
- a bench report against the v0.6 baseline;
- a migration guide;
- the existing quality gates.

Approved decisions (2026-10-14, RESEARCH): D1–D10 in `.maestro/specs/0.7/brief.md`.
The kernel internals are specified separately in `.maestro/specs/0.7/kernel-design-a-hlist.md` and
`.maestro/specs/0.7/kernel-design-b-traitenv.md`. The human picks one at gate H1, after the spike.

See `.maestro/specs/0.7/release-plan.md` for:
- waves, per-leaf contract paths, acceptance criteria and gates;
- the K boundary and exit contract;
- the hot-file register;
- risks and open questions Q1–Q13.

Human gates:
- **H0:** plan approval.
- **H1:** kernel design.
- **H2:** D9 waiver, if needed.
- **H3:** tag and publish to crates.io.

## Decompose batch (for maestro_mission_decompose)

`blocked_by` is not set by decompose. The edges live in the release plan §2/§4 and must be carried
into `.maestro/missions/id-effect-runtime-0-7.execution.md` when the mission is materialized.

| Wave | Slugs |
|---|---|
| 0 | b0-bench-core, b0-bench-fiber, b0-bench-axum, b0-bench-tooling, b0-baseline-capture |
| 1 | w1-labs-pin, w1-drop-shim-crates, w1-request-resolver, w1-defect-type, w1-prune-root-exports, w1-prune-deprecated, w1-prune-macros |
| 2 | k-spike, w2-spike-gate, w2-adr-0011, w2-adr-0003-amend, w2-adr-0007-amend, w2-adr-0008-amend, w2-adr-0010-amend |
| 3 | w3-quarantine, k-rebuild, w3-scaffold, c-race-timeout, c-zip-par-all, c-join-binds, c-fabric-scheduler, c-pure-constructors, c-panic-boundary, c-no-blocking-core, c-tokio-optional |
| 4 | p-layer-core, p-layer-scoped, p-schedule, p-metrics, p-request-batch, p-request-cache, p-random, p-schema-complete, p-config-unify |
| 5 | m-tokio, m-streams, m-config, m-process, m-cli, m-optics, m-fp, m-platform, m-logger, m-resilience, m-sql, m-fsm, m-node, m-otel, m-axum, m-quarantine-exit |
| 6 | s-scaffold, s-domain, s-otel, s-it |
| 7 | r-bench-report, r-migration-guide, r-docs-sweep, r-quality-gates, r-version-bump, r-release |

`k-spike` and `k-rebuild` are placeholders. After H1, `maestro_task_split` expands them into the leaves of the
chosen kernel spec.
