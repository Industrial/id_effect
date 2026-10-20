---
title: 'Wave 8 — Level audit for web.rpc, web.realtime and dist.tls: r12-w8-level-audit'
slug: r12-w8-level-audit
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'For web.rpc (#1–#8), web.realtime (#1–#8) and dist.tls (#1–#8): every planned check id maps to an existing path at HEAD, a named test that passes, a book chapter and a runnable example (METHOD §7.2); level production is evidenced by the reference-relay and reference-ledger CI jobs; the evidence map is attached to the leaf evidence.'
  - 'dist.tls is re-checked after the mesh moved onto the shared endpoint (r12-e12-3-quic-endpoint): #2 #3 #4 #5 #7 still met on both endpoint shapes.'
  - 'Every gap becomes a Maestro task before exit; no file is edited by this leaf.'
  - 'Gate: `jq -r ''.rows[] | select(.id=="web.rpc" or .id=="web.realtime" or .id=="dist.tls") | .checks[].id'' docs/scorecard/scorecard.json` → each id present in the evidence map with a path that exists (`test -e`) and a test that passes (`cargo nextest run -E ''test(<name>)''`)'
  - 'Owns: none (read-only)'
---

# r12-w8-level-audit

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.5 — Consumers and proof". The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 8 (last, with r12-w8-kpis). Blocked by: r12-w7-relay and r12-w7-ledger. Feeds the blind
re-score at exit.
