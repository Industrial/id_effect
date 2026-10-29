# release-0-14 — execution overlay

Mission: `pln-mvtj1svc-m1yw2w` (`maestro mission from-spec .maestro/specs/releases/0.14.0.md`, decomposed
into 38 tasks, one per `.maestro/specs/releases/0.14/r14-*.md` leaf) · spec
`.maestro/specs/releases/0.14.0.md` (approved under the owner's L15 goal of 2026-10-18; every "Taken
(default)" row stands until the owner overturns it; owner decisions D2 D5 D8 D9 D10 of 2026-10-29 recorded; 10 open questions remain) · branch `release/0.14.0` (off
main at `v0.13.0`, 0ae051c3, 2026-10-29).

## Preconditions (day 0) — done at kickoff

- `v0.13.0` tagged and the head of main; `release/0.14.0` branched from it.
- `bash scripts/scorecard.sh --validate --self-test` passes; `--version 0.13.0` reproduces E 86.8 (#1),
  RT 88.7, red 3.
- The 0.13 mission `pln-mvhf4op9-yl99n0` was cancelled ("delivered in v0.13.0") and the five stray tasks
  (r12-f-client-batching, four r13-g-* level-audit gaps) abandoned with their reasons, so `maestro
  status` shows only this mission.
- Every leaf spec passes `maestro spec validate`; the planning scripts are committed beside the leaves
  (`plan-targets.jq`, `open-checks.jq`, `row-moves.txt`).
- Live-test leaves start their own containers through `id_effect_test` on distinct ports.

## Wave 0 — head (dispatch at once)

| Leaf (task id) | Files (owned, summary) | Note |
|---|---|---|
| `r14-t0-adr0016` (`tsk-mvtj22h3-64bbte`) | docs/adrs/0016-sweep-promote-warden.md, scorecard.json `targets["0.14.0"]`, index.html, PLAN-OF-PLANS §6/§11, release spec `scorecard:` block | the Security-table rule numbers were already corrected at kickoff (R4/R7/R8/R10); the leaf only confirms them |
| `r14-t0-skeletons` (`tsk-mvtj22h3-cme3vy`) | ADR 0010 Amendment 10 (incl. the id_effect_fp exception, Open question 6), root Cargo.toml, id_effect_kafka/amqp + warden skeletons, packaging order, kit features `kafka` `amqp` `fsm` + `full`, ci.yml `breadth` job, MIGRATION-0.14 headings | blocks kafka, amqp, warden-core and every leaf that appends to the kit |
| `r14-f-sqlite-per-row` (`tsk-mvtj22h3-kdvszu`) | sql `sqlite/value.rs`, `repository/relations.rs` (key path), gates.toml orm block | benches: never at the same time as core-errors |
| `r14-s-core-errors` (`tsk-mvtj22h3-muqac7`) | core `failure/**`, `kernel/{cause,runloop}.rs` (panic path) | strict tier; kernel bench subset vs v0.13 |

## Wave 1 (after skeletons)

| Leaf (task id) | Depends on / note |
|---|---|
| `r14-s-core-runtime` (`tsk-mvtj22h3-iir88d`) | core-errors; adds on_interrupt/on_exit (never landed), public race_with/disconnect, 1M idle fibers |
| `r14-s-core-di` (`tsk-mvtj22h3-652dv4`) | skeletons; kit `graph.rs` + tool `doctor/graph.rs` only |
| `r14-s-data-fp` (`tsk-mvtj22h3-swwxcr`) | skeletons (Amendment 10 exception) |
| `r14-s-events-upcast` (`tsk-mvtj22h3-34m5zn`) | skeletons; journal column migration |
| ★`r14-s-kafka` (`tsk-mvtj22h3-563we9`) | skeletons — **critical path**; dispatch first: must build on the Windows `matrix` leg (D9, no exclusion) |
| `r14-s-amqp` (`tsk-mvtj22h3-ye85fh`) | skeletons; deny.toml BSD-2-Clause line |
| `r14-s-testing` (`tsk-mvtj22h3-3qm28i`) | skeletons; `mock!`, seeded order harness |
| `r14-s-governance` (`tsk-mvtj22h3-i2jh1z`) | adr0016; docs + `security-kpis.sh --only` |

kafka and amqp both append to `crates/id_effect_test/src/{lib.rs,containers/mod.rs}` (module lines).

## Wave 2

| Leaf (task id) | Depends on / note |
|---|---|
| `r14-s-parallel-supervision` (`tsk-mvtj22h3-2aqheo`) | core-runtime; semver break in process specs → MIGRATION |
| `r14-s-cron-persistent` (`tsk-mvtj22h3-ux5ros`) | — |
| `r14-s-mail` (`tsk-mvtj22h3-vpg7f7`) | largest W2 leaf; split #7 with `maestro task split` if needed (cut 2) |
| `r14-s-web-uploads` (`tsk-mvtj22h3-9xquio`) | the only W2 leaf in id_effect_axum |
| `r14-s-authz-views` (`tsk-mvtj22h3-jxzfh2`) | no axum file |
| `r14-f-idempotency-pg` (`tsk-mvtj22h3-83hz1i`) | F53; stores are rpc features (no sql cycle) |
| `r14-s-tool-generate` (`tsk-mvtj22h3-avo50c`) | builds `generate resource` from scratch (never shipped in 0.9) + `db seed` |
| `r14-s-cli-prompts` (`tsk-mvtj22h3-tu475t`) | — |

mail, web-uploads, authz-views and idempotency-pg each append one kit feature line.

## Wave 3 (six leaves plus warden-core; at most five at once — start ★ first)

| Leaf (task id) | Depends on / note |
|---|---|
| ★`r14-w-warden-core` (`tsk-mvtj22h3-ujqbzv`) | skeletons, parallel-supervision — **critical path**; memory broker + Redis first |
| `r14-s-console-profiling` (`tsk-mvtj22h3-25l646`) | — |
| `r14-s-chaos` (`tsk-mvtj22h3-rculeu`) | testing |
| `r14-s-deploy-k8s` (`tsk-mvtj22h3-7t3seu`) | tool-generate; `ci-pass` line append-only |
| `r14-s-dx-docs-lint` (`tsk-mvtj22h3-bafbeu`) | tool-generate |
| `r14-s-dev-error-page` (`tsk-mvtj22h3-nazd45`) | core-errors |
| `r14-f-h3spec` (`tsk-mvtj22h3-r4wpex`) | F62; `http3-conformance` joins `ci-pass` after two green runs |

## Wave 4

| Leaf (task id) | Depends on |
|---|---|
| ★`r14-w-warden-mesh` (`tsk-mvtj22h3-aqq7ue`) | warden-core, kafka, parallel-supervision, core-runtime — **critical path**; split seam `r14-w-warden-concurrency` named in the leaf |
| `r14-p-ledger` (`tsk-mvtj22h3-0c965b`) | data-fp, testing, parallel-supervision |
| `r14-p-relay` (`tsk-mvtj22h3-xzfovk`) | idempotency-pg, h3spec (F59) |
| `r14-e-fuzz-security` (`tsk-mvtj22h3-4hldm3`) | kafka, amqp, events-upcast, web-uploads, mail, console-profiling |

## Wave 5

| Leaf (task id) | Depends on |
|---|---|
| ★`r14-w-warden-ops` (`tsk-mvtj22h3-gdzyd5`) | warden-mesh, console-profiling, chaos — the §6 exit KPI |
| `r14-p-toolchain` (`tsk-mvtj22h3-d8uqca`) | tool-generate, cli-prompts, dx-docs-lint, deploy-k8s, dev-error-page, warden-mesh, p-ledger, p-relay; owns `warden/{Dockerfile,deploy/**}` only |
| `r14-e-benches` (`tsk-mvtj22h3-57aecm`) | sqlite-per-row, core-runtime, kafka, amqp, parallel-supervision, p-relay; idle machine |

## Wave 6 and 7 — exit

| Leaf (task id) | Depends on |
|---|---|
| `r14-x-level-audit` (`tsk-mvtj22h3-gyltxj`) | every promote, bench and fuzz leaf and warden-ops (read-only) |
| `r14-x-kpis` (`tsk-mvtj22h3-9hs3tl`) | benches, fuzz-security, toolchain, warden-ops; idle machine |
| `r14-x-rescore` (`tsk-mvtj22h3-sxid36`) | level-audit, kpis; blind assessor who built no 0.14 leaf (METHOD §6) |
| `r14-t0-migration-guide` (`tsk-mvtj22h3-ra1d4s`) | every leaf; then CHANGELOG Scorecard + Security, owner steps |

Critical path: skeletons → kafka → (parallel-supervision) → warden-core → warden-mesh → warden-ops →
level audit → re-score. The sweep leaves have two or more waves of slack and fill free slots.

## Rules (as 0.13)

- One shared checkout: every leaf's Owns are disjoint within a wave (release spec "File ownership").
  Append-only shared files are listed there; re-read before editing; insert, never rewrite.
- Commits go through a private index only: `GIT_INDEX_FILE=.tmp/idx-<slug>`, `git read-tree HEAD`,
  pathspec-limited `git add`, `git commit --no-verify -s` (DCO), then `git reset -q -- <paths>` on the
  shared index. Never stash, reset --hard, clean, checkout others' files, push or tag.
- Gates set `TMPDIR` under `/data/Code/rust/id_effect/.tmp/<slug>/`. Toolchain commands run as
  `devenv shell -- <cmd>`. Live tests use `ID_EFFECT_REQUIRE_PG=1` / `ID_EFFECT_REQUIRE_DOCKER=1`, fail
  rather than skip, never read the ambient `DATABASE_URL`.
- Every leaf: its gate, `cargo clippy -p <crate> --all-targets --all-features -- -D warnings`, coverage
  ≥ 96 % regions per touched crate (examples report only), `missing_docs` denied, CHANGELOG line,
  MIGRATION-0.14 section per semver break, `dangerous_*` APIs recorded and listed by
  `scripts/check-dangerous.sh`.
- New pre-1.0 dependencies exact-pinned (rdkafka, lapin, pprof, libtest-mimic) and `cargo deny check`
  clean; `cargo tree -d` shows one rcgen, one quinn, one rustls, one tungstenite.
- Two bench leaves never run at the same time (W0: sqlite-per-row, core-errors; W5: benches; W6: kpis).
- Scorecard commands with `--report` / `--html` write files even with `--file`: run them only in the
  leaf that owns those files (adr0016, rescore).
- A build failure in a file another leaf owns: wait and retry (up to ~30 min) before reporting a blocker.
- Never send the owner's email or other personal data to external services.

## Follow-ups

Carried from 0.13 (all have a leaf): F53 → r14-f-idempotency-pg; F59 → r14-p-relay; F62 → r14-f-h3spec;
F66 → r14-f-sqlite-per-row; sec.supply#7 → r14-s-governance + owner step. F63/F64/F65 were fixed in 0.13.

Recorded during 0.14:

| # | From | Follow-up | Owner (default) |
|---|---|---|---|
