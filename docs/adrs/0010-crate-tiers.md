# ADR 0010 — Vision and crate tiers

## Status

Accepted (2026-10-14). Executed by mission `id-effect-consolidation-0-6`
(`.maestro/missions/id-effect-consolidation-0-6.plan.md`).

## Context

By 0.5.0 the workspace had grown to 25 crates and roughly 90k lines. A module-fit
audit (2026-10-13) against the two serious consumers — solana-yield-optimizer
(recorder on `id_effect_process`) and assay (capability testing kit, `run_blocking`,
`id_effect_cli` exit codes) — found:

- Roughly a third of the code has zero downstream users: the core fp modules
  (algebra, foundation, collections, stm), and the jobs, workflow, events, rpc, parse
  and ai crates.
- Several crates export stubs or spikes from their public API (`derive(Fsm)`,
  `KafkaBrokerStub`, `NetworkJournalStub`, `SchemaBridgeStub`, a deterministic OAuth
  client, `Monad::flatten` calling `unimplemented!`).
- Concepts are duplicated: four types named *Supervisor*, two topological sorts,
  `Bulkhead` vs `Semaphore`, two transducers, three observability layers, two
  node-identity types, two outbox implementations, and a workflow wire protocol
  alongside `id_effect_node`.
- Heavy optional dependencies (rdkafka, bundled rusqlite, apalis release
  candidates) shape main CI, with OpenSSL installs and a Windows special case, for
  crates nobody uses.
- The consumers hit runtime defects in core: `run_with` replaces an installed
  fabric, sysinfo telemetry is sampled eagerly, and streams are not pull-based.

Breadth has outrun quality. New features compete with hardening the parts that
people actually use.

## Decision

### Vision

> id_effect is a typed effect runtime for Rust — `Effect<A, E, R>`, capability DI,
> structured concurrency, resilience, pull-based streams and a first-class testing
> kit — plus OTP-grade supervision and distribution, for long-running supervised
> services, daemons and CLIs. It is not a batteries-included web framework.

Every crate has to justify itself against this sentence.

### Tiers

Every workspace package declares its tier in its manifest:

```toml
[package.metadata.id_effect]
tier = "core"   # core | runtime | integration | maintenance | labs | merge | example
```

| Tier | Published | Bar | Crates (target state) |
|---|---|---|---|
| `core` | yes | strict | id_effect (slimmed), id_effect_macro, id_effect_proc_macro, id_effect_tokio, id_effect_config, id_effect_logger, id_effect_resilience |
| `runtime` | yes | strict | id_effect_process, id_effect_node, id_effect_fsm (reworked as a gen_statem-style behaviour) |
| `integration` | yes | standard | id_effect_platform, id_effect_axum (absorbs tower), id_effect_sql (absorbs sql_pg as a `pg` feature), id_effect_opentelemetry, id_effect_cli |
| `maintenance` | yes | frozen | id_effect_optics (assay depends on it), id_effect_fp (new: algebra, foundation, collections and stm moved out of core) |
| `labs` | **no** (`publish = false`, under `labs/`) | builds only | id_effect_rpc, id_effect_ai, id_effect_parse, id_effect_lint, and the durable family: id_effect_workflow, id_effect_events, id_effect_jobs, id_effect_graph |
| `merge` | transitional | none | id_effect_tower → axum, id_effect_sql_pg → sql (`pg` feature) |
| `example` | no | builds | examples/cli-minimal |

#### Amendment 1 (2026-10-14, during W2 planning)

- `id_effect_jobs` moves to labs whole instead of being split. No published crate
  needs its fabric runner, and splitting it would cost more than it delivers while
  nothing consumes it.
- `id_effect_graph` moves to labs rather than merging into the core planner. Its
  only consumer is `id_effect_events`, so leaving the published surface removes the
  duplicate toposort just as well.
- The durable family (workflow, events, jobs, graph) stays as four labs crates. A
  merged `id_effect_durable` is deferred until a consumer promotes it (rule 2).
- `labs/` is a **separate Cargo workspace** with path dependencies on `crates/`.
  Main-workspace commands (`cargo --workspace`, coverage, the CI matrix,
  minimal-versions) never build labs dependencies such as rdkafka, OpenSSL,
  bundled SQLite or apalis. A dedicated CI job builds and tests labs.
- `id_effect_fsm`'s durable bridge (`fsm/src/workflow.rs`) moves into
  `labs/id_effect_workflow`, so the runtime-tier fsm crate has no labs dependency.

#### Amendment 2 (2026-10-14, after w2-core-slim)

- `id_effect_fp` exists (maintenance tier). It holds `algebra`, `collections`,
  `stm`, `kernel::{result, reader, thunk}` and the `foundation` pieces core does not
  use. The `foundation` parts core uses throughout (`either`, `func`, `piping`,
  `predicate`, `mutable_ref`) stay in core.
- Placement (`compute/cluster.rs`) moved into `id_effect_node::placement`; core
  never used it.
- **Deviation:** tokio remains a required core dependency. The coordination
  primitives (Queue, Deferred, Semaphore, PubSub, …) are built on `tokio::sync`, and
  `FiberHandle` combinators spawn on Tokio. The features are cut to
  `macros, rt, sync`. Making tokio optional would mean a runtime-agnostic
  coordination layer, which is post-gate extension work.
- `im` was removed from core rather than made optional. One consequence is that
  `Cache` now clones a std map on every modify, where `im` used structural sharing.
  This is tracked as a performance follow-up.

The requirements for each tier are in [`docs/QUALITY_BAR.md`](../QUALITY_BAR.md).
`scripts/tier-conformance.sh` checks them.

### Rules

1. A crate enters `core`/`runtime`/`integration` only if it passes the conformance
   check at that tier.
2. A `labs` crate is promoted only when a real consumer depends on it and it passes
   the bar for its target tier.
3. Merges ship `#[deprecated]` re-export shims for one minor release, and the old
   crate is then removed.
4. Labs crates are excluded from `publish.yml`, the 95 % coverage floor and the
   default CI matrix. They build in a separate CI job.
5. New published crates require an ADR amendment that names their tier.

## Consequences

- The published surface shrinks from 25 crates to 15 (plus 2 maintenance), and most
  heavy optional dependencies leave main CI.
- Downstream sees one breaking release (0.6.0). Deprecation shims and a migration
  guide soften it. 0.5.0 is not published (decision D4).
- Durable execution (workflow, events, outbox) leaves the supported surface. If a
  consumer appears, it returns through rule 2.
- Extension work resumes only after the mission's exit gate: all tier 0–2 crates
  conform, coverage is at least 95 % on those tiers, 0.6.0 is published, and assay
  and solana-yield-optimizer build against it.

## Alternatives considered

- **Keep everything, harden everything.** Rejected: the hardening cost is spread
  across code with no users, and the CI weight stays.
- **Separate `id_effect-labs` repository.** Rejected for now (decision D3). Changes
  to core APIs would then need coordinated edits across repositories. An in-repo
  `labs/` folder keeps the history and keeps refactors atomic.
- **Delete unused crates outright.** Rejected: labs keeps the work recoverable at
  near-zero cost.
