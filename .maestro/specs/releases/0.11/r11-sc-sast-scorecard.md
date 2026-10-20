---
title: 'E11.3 — CodeQL, OpenSSF Scorecard and Best Practices: r11-sc-sast-scorecard'
slug: r11-sc-sast-scorecard
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'S4 (SAST, 0.11.0.md D14): .github/workflows/codeql.yml runs CodeQL for Rust and GitHub Actions on push, pull request and weekly, uploads SARIF, SHA-pinned, top-level permissions read; no open high-severity alert at exit.'
  - '.github/workflows/scorecard.yml runs ossf/scorecard-action weekly and on push to main with publish_results: true; the README shows the Scorecard badge.'
  - 'docs/security/openssf.md lists every Scorecard check with its expected score, the evidence, and the owner actions it needs (Branch-Protection, Code-Review, Signed-Releases, Token-Permissions; 0.11.0.md D15), adding up to ≥ 7.5; it also holds the drafted OpenSSF Best Practices (bestpractices.dev) Passing answers with evidence links. The owner submits the questionnaire (D16); the badge goes in the README once Passing.'
  - 'Gate: `bash scripts/check-workflows.sh`; `actionlint .github/workflows/codeql.yml .github/workflows/scorecard.yml`; the Scorecard score from the workflow run (or `scorecard --repo=github.com/Industrial/id_effect`) recorded at exit, ≥ 7.5'
  - 'Owns: `.github/workflows/{codeql.yml,scorecard.yml}`, `README.md` (badges line), `docs/security/openssf.md`'
---

# r11-sc-sast-scorecard

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.3 — Supply chain and badges". The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: the wave-1 head. The ≥ 7.5 result depends on the owner actions in D15/D16
(0.11.0.md Open question 3).
