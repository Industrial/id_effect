---
title: 'Wave 3 — Level and evidence audit (read-only): r11-w3-level-audit'
slug: r11-w3-level-audit
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'For fault.workflows, fault.actors, fault.fsm, dist.tls, dist.placement, dist.membership, dist.remote, dx.lint and the bonus rows conc.stm, sec.supply, test.qa: every planned check id maps to an existing path, symbol or test at HEAD, and each row that must be documented has a book chapter, a runnable example and an integration test (METHOD §7.2); gaps become Maestro tasks before exit.'
  - 'For every non-goal check of method 2.2 the audit confirms it is not counted, and records the dx.repl#3 effect for the report (0.11.0.md D3).'
  - 'Gate: `python3 -I .tmp/plan11/rows.py docs/scorecard/scorecard.json fault. dist. dx.lint conc.stm sec.supply test.qa --checks` compared with the evidence map attached to the leaf'
  - 'Owns: none (read-only)'
---

# r11-w3-level-audit

Leaf of `.maestro/specs/releases/0.11.0.md`, section "Wave 3 — integration and proof". The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 3, after every row-moving leaf.
