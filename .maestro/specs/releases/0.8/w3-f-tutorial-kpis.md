---
title: "0.8 W3-F — zero-to-production tutorial and DX KPI measurement (E1.6, K1/K4/K5)"
slug: r08-w3-f-tutorial-kpis
mode: light
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Book page crates/id_effect/book/src/tutorial-zero-to-production.md (report the SUMMARY line; placed near the start of the book): install id-effect, `id-effect new` a service, run it, add a route with a typed error and a capability, add a test, add --db pg and a migration, run `id-effect doctor` and `--describe-layers`, build the Docker image, deploy config; every Rust step compiled via examples/book-snippets (or verified by a CI script that executes the tutorial steps).'
  - 'scripts/dx-kpis.sh (bash, Requires header, exit 1 FAIL / 2 misuse, --self-test, --json) measures K1 (wall time from `id-effect new svc --template service --local-workspace` to GET /health = 200, cold build in a fresh target dir; budget 180 s for 0.8), K4 (gallery cases in crates/id_effect/tests/ui/gallery whose first error carries a fix note per docs/diagnostics.md — 17/20 expected, owner-accepted), K5 (compiled rust fences / all rust fences in the book, counting the book_snippets harness and the id_effect doctest includes; >= 80 %); writes docs/benchmarks/dx-kpis-0.8.json; a moon task dx-kpis (not in default CI path).'
  - 'Stale docs corrected: crates/id_effect/book/src/part6/ch34-00-dx-ship.md (no longer "no id-effect new", no deploy-kamal path), docs/platform/PHASE_E_CLI_PARITY.md ("No app generator" / id-effect-diagnose references); the orphaned crates/id_effect_cli/templates/admin-stub removed or documented as intentionally kept.'
  - 'cargo test --doc -p book_snippets green; moon run :book green; shellcheck and --self-test for dx-kpis.sh; measured K1/K4/K5 reported with machine info.'
non_goals:
  - The 0.8 blind re-score and release report (exit gate, after wave 3).
---

# 0.8 W3-F — tutorial and DX KPIs

Rows dx.docs, dx.scaffold; KPIs K1, K4, K5. Release spec `.maestro/specs/releases/0.8.0.md`.
