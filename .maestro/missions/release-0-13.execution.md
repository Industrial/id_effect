# release-0-13 — execution overlay

Mission: `pln-mvhf4op9-yl99n0` (`maestro mission from-spec .maestro/specs/releases/0.13.0.md`, decomposed
into 39 tasks, one per `.maestro/specs/releases/0.13/r13-*.md` leaf) · spec
`.maestro/specs/releases/0.13.0.md` (approved under the owner's L15 goal of 2026-10-18; every "Taken
(default)" row stands, owner steps D16/D17 open) · branch `release/0.13.0` (off main after `v0.12.0` was
tagged; the owner confirmed the prerequisite on 2026-10-21).

## Preconditions (day 0)

- `v0.12.0` tagged and on main (confirmed 2026-10-21). `docs/scorecard/results/v0.12.0.json` and
  `REPORT-0.12.0.md`: r13-t0-adr0015 re-checks them and fills `carry_over`.
- Owner has seen the nine open questions in the release spec (defaults apply if unanswered).
- Live-test leaves start their own containers through `id_effect_test` on distinct ports.

## Wave 0 — head (dispatched at kickoff)

| Agent | Leaf (task id) | Files (owned) |
|---|---|---|
| A0 roadmap | `r13-t0-adr0015` (`tsk-mvhf59ri-gcmaue`) | docs/adrs/0015-breadth-batteries.md, PLAN-OF-PLANS.md §5 T9 / §6 / §11, the release spec `scorecard:` block |
| A1 governance | `r13-t0-skeletons` (`tsk-mvhf59ri-8vwj0w`) | docs/adrs/0010-crate-tiers.md (Amendment 9), root Cargo.toml, labs/Cargo.toml, `git mv labs/id_effect_ai crates/id_effect_ai`, crates/id_effect_{admin,i18n}/ skeletons, examples/reference/pipeline/ skeleton, crates/id_effect_cli/templates/admin-stub/** (deleted), scripts/validate-crate-packaging.sh, ci.yml (`-p` lines, labs job), docs/MIGRATION-0.13.md (headings) |
| A2 deps | `r13-f-rcgen` (`tsk-mvhf59ri-2mysjl`) | rcgen lines of six manifests, the 19 rcgen `.rs` files, deny.toml |
| A3 core | `r13-f-any-env` (`tsk-mvhf59ri-0jkcwz`) | crates/id_effect/src/kernel/{effect,local}.rs (`any_env` only), tests/any_env.rs, one book section |

## Wave 1 (after skeletons)

| Leaf (task id) | Files (owned) | Depends on / note |
|---|---|---|
| ★`r13-e9-5-views` (`tsk-mvhf59ri-2uwyi9`) | crates/id_effect_axum/{Cargo.toml (ui, maud), src/ui/**, assets/**}, tests/ui.rs, example 095, book ch29-10 | skeletons — **critical path** |
| ★`r13-e9-1-promote` (`tsk-mvhf59ri-55inb3`) | crates/id_effect_ai/** (declares every 0.13 feature once), book ch35-00 | skeletons (ai move) |
| `r13-e3-3-relations` (`tsk-mvhf59ri-o3mnz0`) | sql `repository/{mod,relations}.rs` (+ empty page/hooks), proc_macro repository.rs, tests, book ch28-06 | owns the `Entity::FIELDS` contract admin depends on |
| `r13-e9-4-i18n-core` (`tsk-mvhf59ri-d7qwhu`) | crates/id_effect_i18n/** (core), book ch30-09 | skeletons |
| `r13-f-client-batching` (`tsk-mvhf59ri-9x4e24`) | rpc `src/{service.rs,client/http.rs}`, tests/service_macro.rs (new tests) | any-env; applies `0.13/f27-client-batching.patch` |
| `r13-f-stream-chunking` (`tsk-mvhf59ri-jpxbhs`) | crates/id_effect/src/streaming/{stream,pump}.rs (pull path) | strict tier; bench compare vs v0.12 |
| `r13-f-eventlog-head` (`tsk-mvhf59ri-64yymd`) | events `{subscription,sql_journal,event_store}.rs`, rpc `subscription/events.rs`, tests/head.rs | — |

## Wave 2

| Leaf (task id) | Depends on |
|---|---|
| ★`r13-e9-3-admin-core` (`tsk-mvhf59ri-hqb6sh`) | views, skeletons, relations (`Entity::FIELDS`) — **critical path**; leaf most likely to be wrong |
| ★`r13-e9-1-tools` (`tsk-mvhf59ri-7mq6sz`) | promote |
| `r13-e9-1-embeddings` (`tsk-mvhf59ri-i60zro`) | promote |
| `r13-e3-3-pagination` (`tsk-mvhf59ri-5gimx6`) | relations |
| `r13-e3-3-hooks` (`tsk-mvhf59ri-d82b9b`) | relations; own Postgres container |
| `r13-e9-4-i18n-web` (`tsk-mvhf59ri-1ru5me`) | i18n-core |
| `r13-e5-5-tenancy` (`tsk-mvhf59ri-ua2lw4`) | relations (sql `lib.rs` line); own Postgres container |

## Wave 3 (seven leaves; at most five at once — start the ★ leaves first)

| Leaf (task id) | Depends on |
|---|---|
| ★`r13-e9-3-admin-edit` (`tsk-mvhf59ri-47pqck`) | admin-core, relations, pagination, hooks — **critical path** |
| ★`r13-e9-1-mcp` (`tsk-mvhf59ri-07dfve`) | tools |
| `r13-f-h3-settings` (`tsk-mvhf59ri-xpedhx`) | rcgen |
| `r13-f-ws-credit` (`tsk-mvhf59ri-bvwb9u`) | — |
| `r13-f-mesh-session` (`tsk-mvhf59ri-pdbe88`) | — |
| `r13-f-ws-client-tls` (`tsk-mvhf59ri-saufjy`) | rcgen |
| `r13-e9-5-leptos` (`tsk-mvhf59ri-c8i4zl`) | views; first in the scope-cut order |

## Wave 4 (six leaves; at most five at once)

| Leaf (task id) | Depends on |
|---|---|
| ★`r13-e1-3-generate-admin` (`tsk-mvhf59ri-x2qmeh`) | admin-edit — **critical path** |
| `r13-e-fuzz-security` (`tsk-mvhf59ri-a8w9hx`) | mcp, tools, i18n-web, pagination, admin-edit; TMPDIR under /data |
| `r13-e-benches` (`tsk-mvhf59ri-j3gf9e`) | relations, pagination, i18n-core, views, stream-chunking; idle machine; budgets after measuring (D15) |
| `r13-f-webtransport-host` (`tsk-mvhf59ri-ye7a0y`) | h3-settings (Risk 6: Deno may be infeasible) |
| `r13-f-ext-connect` (`tsk-mvhf59ri-l9ih4g`) | ws-credit (shares `realtime/ws.rs`) |
| `r13-f-rpc-retire` (`tsk-mvhf59ri-5gvyft`) | — |

## Wave 5

| Leaf (task id) | Depends on |
|---|---|
| ★`r13-w5-kit-template` (`tsk-mvhf59ri-tdjahc`) | generate-admin, mcp, embeddings, i18n-web, tenancy, hooks, views — **critical path** |

## Wave 6

| Leaf (task id) | Depends on |
|---|---|
| ★`r13-w6-pipeline` (`tsk-mvhf59ri-xlpdlx`) | kit-template — §6 KPI, never cut; owns the `reference-pipeline` CI job |
| `r13-f-relay-host` (`tsk-mvhf59ri-v0qnuc`) | kit-template, webtransport-host, ws-client-tls, client-batching, mesh-session, ext-connect; relay steps in ci.yml rebased after pipeline |

## Wave 7

| Leaf (task id) | Depends on |
|---|---|
| ★`r13-w7-pipeline-backoffice` (`tsk-mvhf59ri-6qnxm6`) | pipeline (its CI job green first) |

## Wave 8 and exit

| Leaf (task id) | Depends on |
|---|---|
| `r13-w8-level-audit` (`tsk-mvhf59ri-fjrbf4`) | pipeline-backoffice, relay-host (read-only) |
| `r13-w8-kpis` (`tsk-mvhf59ri-pwqw3c`) | pipeline-backoffice, benches, fuzz-security; idle machine; last |
| `r13-x-rescore` (`tsk-mvhf59ri-bpu8pp`) | every leaf; blind assessor who did not build the leaves (METHOD §6) |
| `r13-t0-migration-guide` (`tsk-mvhf59ri-pqqwbl`) | every leaf; then REPORT-0.13.0, CHANGELOG Scorecard + Security, owner steps D16 (ADR 0015 accepted) / D17 (crates.io) |

Critical path: skeletons → views → admin-core → admin-edit → generate-admin → kit-template → pipeline →
pipeline-backoffice → level audit → re-score. The AI branch (promote → tools → mcp → kit-template) has
one wave of slack; the T12 carry leaves have three or more and fill free slots.

## Rules (as 0.12, plus the 0.12 lessons)

- One shared checkout: every leaf's Owns are disjoint within a wave (release spec "File ownership").
  Append-only shared files: `crates/id_effect/book/src/SUMMARY.md`, `examples/book-snippets/src/part6.rs`,
  CHANGELOG.md, docs/MIGRATION-0.13.md, Cargo.lock (regenerated), the `lib.rs` of id_effect_{ai,sql,
  axum,authz,i18n,admin} after their wave-0/1 owner, and the module-declaration lines of sql
  `repository/mod.rs`, axum `server/mod.rs`, rpc `{client,transport}/mod.rs`. Re-read before editing;
  insert, never rewrite.
- Commits go through a private index only: `GIT_INDEX_FILE=.tmp/idx-<slug>`, `git read-tree HEAD`,
  pathspec-limited `git add`, `git commit --no-verify -s` (DCO), then `git reset -q -- <paths>` on the
  shared index. Never stash, reset --hard, clean, checkout others' files, push or tag.
- Gates set `TMPDIR` under `/data/Code/rust/id_effect/.tmp/<slug>/` (the `/tmp` tmpfs fills). Live tests
  use `ID_EFFECT_REQUIRE_PG=1` / `ID_EFFECT_REQUIRE_DOCKER=1`, fail rather than skip, never read the
  ambient `DATABASE_URL`.
- Every leaf: its gate, `cargo clippy -p <crate> --all-targets --all-features -- -D warnings`, coverage
  ≥ 96 % regions per touched crate (examples outside the gate, 0.12 F54), `missing_docs` denied,
  CHANGELOG line, MIGRATION-0.13 section per semver break, `dangerous_*` APIs recorded and listed by
  `scripts/check-dangerous.sh`.
- New pre-1.0 dependencies exact-pinned (maud, fluent-*, htmx file sha384) and `cargo deny check` clean;
  `cargo tree -d` shows one rcgen, one quinn, one rustls, one tungstenite.
- A build failure in a file another leaf owns: wait and retry (up to ~30 min) before reporting a blocker.
- Never send the owner's email or other personal data to external services.

## Follow-ups

Carried from the 0.12 overlay (rows the release spec does not give to a 0.13 leaf):

| # | From | Follow-up | Owner (default) |
|---|---|---|---|
| F63 | 0.12 CI (PR #112) | flaky: `id_effect_sql::conformance mysql::dropped_streams_release_their_connections` failed once in Container integration (2627692d: `SELECT 1 AS n: sql client not connected` after 88 s, pool acquire timed out); passed on aa84c378 and #111 with no sql source change. Make it deterministic (wait for the pool's idle count instead of a fixed timeout; check the MySQL container's `max_connections`) | **fixed** in e2a7977 with 0.12 F65: a dropped MySQL stream closes its connection after `KILL QUERY` instead of the pool draining the rest, so the test needs no retry or long acquire timeout (100 000-row streams, release within 20 s). 0.12 F64 (Windows first sysinfo sample) fixed in 556b562. |
| F62 | 0.12 CI (PR #112) | h3spec failures in the `server/h3.rs` adapter (connection-specific headers, pseudo-header placement, field-section size) plus upstream h3 / quinn items; `http3-conformance` not in `ci-pass` | coordinator: adapter part after r13-f-h3-settings merges (same file), else 0.14 |
| F53 | ledger/relay | two Postgres idempotency stores | 0.14 (explicit, not 0.13 scope) |
| F59 | relay | feature exercise (streams, SSE, postcard, batching, WS resume) | 0.14 E13.2 (0.13 part in r13-f-relay-host) |

Recorded during 0.13:

| # | From | Follow-up | Owner (default) |
|---|---|---|---|
| F66 | r13-e-benches (owner decision 2026-10-24) | SQLite driver per-row cost; eager pair back toward 0.25. The pair `orm/eager_100x10 / orm/n_plus_one_100x10` was raised from 0.25 to 0.45 for 0.13.0 because sqlx-sqlite's per-row cost (about 0.67 µs a row) puts even two plain reads (`orm/flat_100x10`) at 0.25–0.29 of the N+1 loop. Cut the per-row cost in `crates/id_effect_sql/src/sqlite/` (first candidate: `decode` in `value.rs` copies the storage-class name into a `String` per value) and the relation pass, then lower the limit in `docs/benchmarks/gates.toml` (README "Breadth gates (0.13)") | 0.14 (sql; `src/sqlite/` is in no 0.13 leaf's Owns) |
