---
title: 'Proof — Per-check evidence audit at HEAD for every 0.14 target row and every promoted row: r14-x-level-audit'
slug: r14-x-level-audit
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'Scope, 38 rows: the 35 rows with a planned 0.14.0 target (.maestro/specs/releases/0.14/row-moves.txt; the targets r14-t0-adr0016 stores in docs/scorecard/scorecard.json): core.typed-effect, core.di, core.cause, core.interrupt, core.race, conc.fibers, conc.coordination, conc.stm, conc.parallel, fault.supervision, fault.fsm, data.events, data.fp, web.storage, sec.authz, sec.supply, obs.console, msg.cron, msg.brokers, msg.email, test.unit, test.determinism, test.property, test.chaos, dx.scaffold, dx.cli, dx.docs, dx.diagnostics, dx.lint, dx.compile, dx.repl, dx.deploy, eco.stability, eco.backing, eco.fit; plus the three promoted rows whose score does not move (core.ergonomics, conc.perf, dist.membership: documented → production at 88, no 0.14 target). Re-checked, kept: web.rpc and web.realtime (F59 in the Relay, F53, F62 h3spec) and data.orm (F66).'
  - 'Per planned check id (the sweep list of .maestro/specs/releases/0.14/plan-targets.jq plus every check already met at 0.13.0): an existing path at HEAD (`test -e`), a named test that passes (`cargo nextest run -E ''test(=<name>)''` in the crate that holds it), a book chapter and a runnable example (METHOD §7.2, docs/scorecard/METHOD.md:262-283; level `documented` needs all four, table :278).'
  - 'Per promoted row (D3, the 19 rows at level documented in 0.13.0): the reference app (Ledger, Relay, Pipeline or Warden), the CI job (`reference-ledger`, `reference-relay`, `reference-pipeline`, `reference-warden`, `reference-toolchain`) and the test name(s) that exercise each met check, taken from the apps'' README evidence tables (r14-p-ledger, r14-p-relay, r14-p-toolchain, the Warden leaves) and confirmed green in the CI run of the release candidate commit (run id recorded); a row without such a test stays documented in the map.'
  - 'Checks that this release cannot meet are recorded as such, not as gaps: core.ergonomics#6 (0.17 E13.6), conc.perf#5 (external listing), dist.membership#8 (0.16 E13.5), dx.diagnostics#6, and the eco.* owner checks; each with the release that owns it.'
  - 'The evidence map (row → check → path, test, chapter, example; promoted rows → app, job, test, run id) is attached to the leaf evidence for r14-x-rescore; every gap becomes a Maestro task before exit; this leaf writes no score, level or target and edits no file.'
  - 'Tier: read-only audit. Secure by default: not applicable (no surface); the sec.* rows are audited like the others.'
  - 'Coverage: not applicable (no code).'
  - 'Gate: `f=docs/scorecard/scorecard.json; jq -r ''.rows[] | select(.targets["0.14.0"] != null or .v2.level.id_effect["0.13.0"] == "documented" or .id == "web.rpc" or .id == "web.realtime" or .id == "data.orm") | .checks[] | select(.non_goal | not) | .id'' $f` → each id present in the evidence map, met or explicitly recorded as open; each cited path passes `test -e`; each cited test passes `cargo nextest run -E ''test(=<name>)''`; `bash scripts/scorecard.sh --validate`'
  - 'Owns: none (read-only)'
---

# r14-x-level-audit

Leaf of `.maestro/specs/releases/0.14.0.md`, section "Proof". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 6 (with r14-x-kpis). Blocked by: r14-p-ledger, r14-p-relay, r14-p-toolchain, r14-w-warden-ops
(and through it the Warden leaves), r14-e-benches, r14-e-fuzz-security, and every W1–W3 sweep leaf
whose check it audits. Feeds r14-x-rescore.

Defaults the owner may overturn: the auditor reads the stored 0.14.0 targets, not the coordinator's
copy (.tmp/r14/scorecard-0.14.json), so the gate passes only once r14-t0-adr0016 (or
r14-t0-skeletons) has written them; the three promoted rows whose score stays 88 are in scope
because D3 is about level, not score; `reference-warden` is the name the Warden leaves are expected
to give their CI job.
