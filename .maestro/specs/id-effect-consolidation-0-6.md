---
title: id_effect 0.6 Consolidation — narrow, harden, then extend
slug: id-effect-consolidation-0-6
mode: heavy
work_type: initiative
risk_class: high
version: 1
acceptance_criteria:
  - Workspace declares rust-version in workspace.package inherited by every crate, CI has a green MSRV job on that toolchain, and dependabot ignores sysinfo >= 0.34
  - ADR-0010 records the vision and crate tier table, docs/QUALITY_BAR.md defines per-tier requirements, and scripts/tier-conformance.sh reports per-crate pass/fail from package.metadata.id_effect.tier
  - A pre-installed ComputeFabric survives run, run_with, ThreadSleepRuntime and id_effect_tokio default runtime, proven by regression tests
  - SysinfoTelemetry is not sampled until a placement decision needs it and a criterion startup bench for run_with stays under 50 ms
  - Stream is pull-based over bounded chunks; window, join, state_scan, transducer and map_effect emit incrementally on infinite sources; an assert_bounded_memory test helper exists and a proptest shows infinite source plus take(n) terminates
  - ADR-0008 and code agree on run_blocking_serial and stream mapping (implemented or ADR amended)
  - rpc, ai, parse, lint and a new id_effect_durable (workflow + events + jobs outbox/broker adapters) live under labs/ with publish = false, are excluded from publish.yml, the coverage floor and the default CI matrix, and main CI no longer needs rdkafka, OpenSSL or the Windows all-features special case
  - tower is merged into axum, sql_pg into sql as a pg feature, graph into the core capability planner, jobs split (fabric runner to node, adapters to durable), each with one release of deprecated re-export shims
  - Core id_effect is slimmed — algebra, foundation, collections and stm move to a maintenance-tier id_effect_fp crate; compute is behind a feature with cluster placement moved to node; tokio, rayon, sysinfo and im are optional; core has no dev-deps on config or jobs
  - No stub, spike, unimplemented! or todo! is reachable from the public API of any tier 0, 1 or 2 crate
  - ComputeSupervisor is renamed FabricGovernor and platform::process is renamed platform::command with deprecated aliases, and the book documents one supervision layering (core primitive, process, node)
  - id_effect_process, id_effect_fsm, id_effect_axum, id_effect_platform, id_effect_sql, id_effect_cli and id_effect_resilience pass tier-conformance at their tier, including Effect-native fsm with real derive(Fsm), one documented axum Send strategy, and resilience retry/breaker integrated with Schedule
  - README crate table is generated from package metadata and checked in CI; book, PARITY_DASHBOARD, ROADMAPs and CHANGELOG match the code; cookbook recipes and a 0.5 to 0.6 migration guide exist
  - Capability boilerplate reduced (infallible error helper, turbofish-free logger calls, provider derive without clippy allows) and capability diagnostics carry call-site context
  - 0.6.0 is published lockstep for all non-labs crates, templates and examples pin the workspace version under a CI check, and assay and solana-yield-optimizer build against crates.io 0.6.0
non_goals:
  - New features beyond those required to meet the quality bar (extension resumes after the exit gate)
  - Publishing 0.5.0 (decision D4 2026-10-14 - skip straight to 0.6.0)
  - Batteries-included web framework features (ORM, auth, migrations, mailers)
  - Promoting labs crates back to published tiers within this mission
  - Observability unification on tracing (post-gate extension candidate)
---

# id_effect 0.6 Consolidation

Re-focus id_effect on its proven niche: a typed effect runtime plus OTP-grade
supervision for long-running services, daemons and CLIs. Fix the three
downstream-reported runtime defects (run_with fabric overwrite, eager telemetry,
eager streams), re-tier the 25 crates, hold every kept crate to a written quality
bar, make docs truthful, and release 0.6.0 with downstream migrated.

Approved decisions (2026-10-14): D1 vision accepted; D2 tiers accepted as proposed;
D3 labs in-repo under labs/ with publish = false; D4 do not publish 0.5.0.

See .maestro/missions/id-effect-consolidation-0-6.plan.md for waves, per-leaf paths,
AC and gates.
