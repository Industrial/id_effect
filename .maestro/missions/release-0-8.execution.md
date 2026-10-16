# release-0-8 — execution overlay

Mission `pln-mvawbp5i-g5m6ot` · spec `.maestro/specs/releases/0.8.0.md` · leaf specs `.maestro/specs/releases/0.8/`.

## Wave 1

| Agent | Leaves (task id) | Files (owned) |
|---|---|---|
| A1 trust | E0.2 `tsk-mvawkapy-nam47g`, E0.3 `tsk-mvawkapy-h9kogq`, E0.4 `tsk-mvawkapy-xdespa` | SECURITY.md, deny.toml, GOVERNANCE.md, docs/STABILITY.md, docs/MIGRATION-0.8.md (creates), .github/workflows/{ci,publish}.yml, .github/CODEOWNERS, devenv.nix, moon.yml (deny, semver) |
| A2 web | E5.1 `tsk-mvawkapy-tx4pfl` | crates/id_effect_axum/src/server/security*, tests/security.rs, examples/040, Cargo.toml deps, book ch30 |
| A3 metrics | E6.1a `tsk-mvawkapy-uuzduq` | id_effect observability/{prometheus,runtime_metrics}.rs, runtime/fiber_context.rs, axum src/metrics.rs, sql src/metrics.rs, book ch27-01, example 070 |
| A4 health | E6.1b `tsk-mvawkapy-y6r782` | axum src/health.rs, server/lifecycle.rs, sql src/health.rs + pg/client.rs, book ch27-02, example 071 |
| A5 cli | E1.10 `tsk-mvawkapy-uh71va`, E2.6 `tsk-mvawkapy-km3wy8` | crates/id_effect_cli/**, examples/cli-minimal, scripts/compile-time.sh, docs/benchmarks/compile-0.8.json, moon.yml (compile-time) |
| A6 hygiene | E2.1h `tsk-mvawkapy-fwcf2f` | id_effect streaming/{sink,channel}.rs, coordination/{queue,channel}.rs, scheduling/schedule.rs, tests/stream_channel.rs, docs/benchmarks/{gates.toml,REPORT-0.8.md} |
| held | E0.1 `tsk-mvawkapy-lzvg52` | waits for owner consent (Justin Carter #79; IDClear commits) |

Shared files, integrator-only (main session at wave end): CHANGELOG.md, docs/MIGRATION-0.8.md entries
(agents report text), book SUMMARY.md lines, docs/scorecard/scorecard.json evidence. Agents append
their own `pub mod`/`pub use` lines in crate lib.rs files only. Agents never commit or stage; the main
session reviews and commits per leaf (pathspec-limited). examples/http-showcase is owned by a
concurrent session and is off-limits.

## Wave 2

Tasks created with `task from-spec` (not attached to the mission id by the CLI; tracked here).

| Agent | Leaf (task id) | Files (owned) |
|---|---|---|
| B1 cron+resilience | E2.1c+r `tsk-mvb0jm49-3ytgwg` | crates/id_effect/src/scheduling/cron*.rs (new) + scheduling/mod.rs, crates/id_effect_resilience/**, new book pages |
| B2 streams | E2.1s `tsk-mvb0jm8t-w9qmja` | crates/id_effect/src/streaming/** (new operator files preferred), streaming tests, book streams chapter |
| B3 schema | E2.3 `tsk-mvb0jmcb-ajdki6` | crates/id_effect/src/schema/**, schema tests, book schema chapter |
| B4 property | E2.2 `tsk-mvb0jmfu-jljtq9` | crates/id_effect/src/testing/{prop*,assert*,golden*}.rs (new) + testing/mod.rs lines, crates/id_effect/Cargo.toml (proptest/insta features), new book page |
| B5 determinism+idioms | E2.4+E1.8 `tsk-mvb0jmj8-is6qdv` | crates/id_effect/src/testing/test_executor.rs, new book idiom guide page |

Shared, append-only: crates/id_effect/src/lib.rs and testing/mod.rs (own lines only). Integrator-only:
book SUMMARY.md, CHANGELOG.md, docs/MIGRATION-0.8.md, docs/scorecard/.

## Wave 3 (front door)

Owner decisions 2026-10-16: K4 counts a note that states the concrete fix; K5 stays 80 % (all
conversions in 0.8). Follow-up E2.3b `tsk-mvb3399t-pysl4g` shipped as 697098c (derive JSON Schema,
schema-regex). Harness skeleton e7e7ac1.

| Agent | Leaf (task id) | Files (owned) | Depends on |
|---|---|---|---|
| W3-A | kit `tsk-mvb8iicl-xlm756` | crates/id_effect_kit/**, ADR 0010 amendment, members, moon workspace, packaging LEVEL/ORDER, ci.yml semver exclude | — |
| W3-D | gallery `tsk-mvb8iifv-1kp3tn` | crates/id_effect/tests/ui/**, on_unimplemented texts, docs/diagnostics.md | — |
| W3-E1/E2/E3 | snippets `tsk-mvb8iij2-kzsw5d` | book part1-2+cookbook+top / part3-4 / part5-7 and the matching examples/book-snippets/src modules | — |
| W3-B | `id_effect_tool` + `new` + 4 templates + template CI | crates/id_effect_tool/** (not doctor), template smoke script, ci.yml templates job | A |
| W3-C | doctor + layer graph | crates/id_effect_tool/src/doctor/**, crates/id_effect_kit/src/graph.rs | A, B |
| W3-F | tutorial + scripts/dx-kpis.sh + scorecard evidence | tutorial page, dx-kpis.sh, stale CLI docs | B, C, D, E |

## Log

| Date | Event |
|---|---|
| 2026-10-16 | Mission created and decomposed (10 wave-1 leaves). Wave 1 dispatched (A1–A6); E0.1 held for consent. |
| 2026-10-16 | Wave 1 shipped (owner sign-off): e9a4171, 954368f, a36ed4f, 4afcf08, b3d34ac, 782b132. |
| 2026-10-16 | Wave 2 shipped (owner sign-off): 29d0607, 18df824, 2ce6900, 6241df1, dc76461, 51e0928; follow-up 697098c. |
| 2026-10-16 | Wave 3 shipped (owner sign-off): kit, tool + new, doctor + graph, diagnostics gallery (K4 17/20), book snippets 95 %, tutorial + dx-kpis; 90b9c94 (clippy 1.99), c4006a4. |
| 2026-10-16 | E0.1: owner chose to remove the third-party obix outbox (d72d7a8) instead of seeking consent; relicensed to MIT (a26876e). |
| 2026-10-16 | Exit: blind re-score 0.8.0 E 55.2 (#5, target 53.0), REPORT-0.8.0 (11f0a16); coverage 96.15 % regions / 96.12 % functions / 96.56 % lines; version 0.8.0 (8aa63b6). Owner approved release and publishing. |
