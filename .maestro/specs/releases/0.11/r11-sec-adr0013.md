---
title: 'E11.2 — ADR 0013 "Secure by default" and the posture registry: r11-sec-adr0013'
slug: r11-sec-adr0013
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'docs/adrs/0013-secure-by-default.md (status Proposed; the owner moves it to Accepted, 0.11.0.md D22) states the ten rules R1–R10 exactly as the 0.11.0.md table "ADR 0013 — the ten rules", each with the findings it closes, its enforcement point(s) and the leaf that implements it; it defines "closed" for a finding as "the default path is safe and every escape is a named, recorded dangerous_*/insecure_* opt-out that fails the prod posture check" (pending the owner''s answer to Open question 1).'
  - 'id_effect::observability::posture: record_dangerous(id: &''static str, reopens: &''static str) records an opt-out once per id, process-wide; dangerous_opt_outs() lists them; no cost on paths that never call it; documented as the hook every dangerous_*/insecure_* API in the workspace calls.'
  - 'Tests `posture_is_empty_by_default`, `posture_records_each_opt_out_once`, `posture_lists_reason_text`.'
  - 'Gate: `cargo nextest run -p id_effect --test posture`; `cargo nextest run -p id_effect --test kernel_alloc`'
  - 'Owns: `docs/adrs/0013-secure-by-default.md`, `crates/id_effect/src/observability/{posture.rs,mod.rs}`, `crates/id_effect/tests/posture.rs`'
---

# r11-sec-adr0013

Leaf of `.maestro/specs/releases/0.11.0.md`, sections "E11.2 — Secure by default" and "ADR 0013".
The release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1 head (day 0). Blocks every leaf that adds a dangerous_* or insecure_* API. The prod posture
check that reads the registry is r11-w3-kit-template (wave 3).
