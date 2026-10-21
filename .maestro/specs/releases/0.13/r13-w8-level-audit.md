---
title: 'Wave 8 — Level audit for the six breadth rows and the three kept T12 rows: r13-w8-level-audit'
slug: r13-w8-level-audit
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'For data.orm (#3 #7 plus the kept #1 #2 #5 #6 #8), web.ui (#1 #2 #6 #8, margin #5), web.admin (#1 #2 #3 #8, margin #4 #5 #6), ai.llm (#1–#7, margin #8), eco.i18n (#1 #2 #3 #5 #7) and eco.tenancy (#1 #2 #3 #5 #6 #7, margin #8): every planned check id maps to an existing path at HEAD, a named test that passes, a book chapter and a runnable example (METHOD §7.2); level production is evidenced by the reference-pipeline CI job; the evidence map is attached to the leaf evidence.'
  - 'web.rpc, web.realtime and dist.tls are re-checked after the T12 carry leaves (h3 settings, WebSocket changes, mesh session pump, Relay on the host): no check lost; level production still evidenced by reference-relay and reference-ledger.'
  - 'Every gap becomes a Maestro task before exit; no file is edited by this leaf.'
  - 'Tier: read-only audit.'
  - 'Gate: `jq -r ''.rows[] | select(.id=="data.orm" or .id=="web.ui" or .id=="web.admin" or .id=="ai.llm" or .id=="eco.i18n" or .id=="eco.tenancy" or .id=="web.rpc" or .id=="web.realtime" or .id=="dist.tls") | .checks[].id'' docs/scorecard/scorecard.json` → each id present in the evidence map with a path that exists (`test -e`) and a test that passes (`cargo nextest run -E ''test(<name>)''`)'
  - 'Owns: none (read-only)'
---

# r13-w8-level-audit

Leaf of `.maestro/specs/releases/0.13.0.md`, section "Proof". The release spec's row is binding; this
file exists so Maestro can track the leaf as its own task.

Wave 8 (last, with r13-w8-kpis). Blocked by: r13-w7-pipeline-backoffice and r13-f-relay-host. Feeds
r13-x-rescore.
