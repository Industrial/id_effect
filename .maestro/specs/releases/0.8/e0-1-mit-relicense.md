---
title: 0.8 E0.1 — relicense to MIT (L10)
slug: r08-e0-1-mit-relicense
mode: light
work_type: maintenance
risk_class: high
version: 1
acceptance_criteria:
  - 'Precondition recorded as evidence before any change: consent from every non-owner copyright holder (Justin Carter, commit 8eb9b05 / #79) or those lines rewritten; owner confirms IDClear has no claim on the 4 commits made with the idclear.com address (#67, #71, #72, #73).'
  - 'LICENSE at repo root with the MIT text (head -1 = MIT License); license = "MIT" in Cargo.toml:32 [workspace.package], labs/Cargo.toml:23 and labs/id_effect_lint/Cargo.toml:7; cargo metadata shows license MIT for every publishable package in both workspaces.'
  - 'README badge (README.md:19) and License footer (README.md:244-246) and labs/id_effect_lint/README.md:31-33 updated; git grep CC-BY-SA outside docs/scorecard, .maestro and CHANGELOG returns nothing; CHANGELOG notes that versions <= 0.7.0 remain CC-BY-SA-4.0.'
  - 'CONTRIBUTING.md exists with a DCO (Signed-off-by) requirement; cargo package -p id_effect --list succeeds.'
non_goals:
  - Re-licensing already published versions.
---

# 0.8 E0.1 — MIT relicense

Moves eco.license 1/5 -> 5/5 (process row, METHOD 2.1). Release spec: `.maestro/specs/releases/0.8.0.md`.
