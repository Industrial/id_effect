---
title: 0.8 E0.3 — cargo-semver-checks in CI and a written stability policy
slug: r08-e0-3-stability
mode: light
work_type: maintenance
risk_class: high
version: 1
acceptance_criteria:
  - 'A semver job in .github/workflows/ci.yml runs cargo semver-checks for the published crates against the 0.7.0 registry baseline; intentional 0.8 breaks are allowed only when listed in docs/MIGRATION-0.8.md (the job compares, or is report-only until the 0.8 tag and enforcing after it — documented in the job); job listed in ci-pass.needs; moon task semver; cargo-semver-checks in devenv.nix.'
  - 'docs/STABILITY.md (linked from README and QUALITY_BAR): deprecation rule (one-minor #[deprecated] shim + migration entry), semver policy before/after 1.0, MSRV policy stated as "at most N-2 stable" consistent with rust-version = 1.94 and the msrv CI job.'
  - 'docs/MIGRATION-0.8.md created (header + sections that wave leaves append to).'
non_goals:
  - LTS lines (1.0).
---

# 0.8 E0.3 — stability

Adds eco.stability #6 (5/8 = 63, above the 0.8 target 50). Release spec: `.maestro/specs/releases/0.8.0.md`.
