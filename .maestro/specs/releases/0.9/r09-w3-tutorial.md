---
title: 'Tutorial: zero to production on the 0.9 service template'
slug: r09-w3-tutorial
mode: light
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'book/src/tutorial-zero-to-production.md follows the 0.9 service template: migrations through id_effect_sql::migrate (migrations/ applied on start, id-effect migrate new / status / check), no include_str! migration constants; the typed route and Problem+JSON answer; the middleware stack.'
  - 'Every rust block still compiles in examples/book-snippets (cargo test -p book_snippets --doc); the tutorial text matches the generated project file by file (verify by generating one with id-effect new and comparing).'
  - 'dx KPI K1 (time to a running service) is unaffected; scripts/dx-kpis.sh still passes its tutorial-related steps if any.'
---

# r09-w3-tutorial

The kit/template leaf replaced the template's `include_str!` migration with `migrate::run`; the
tutorial still teaches the old way (tutorial-zero-to-production.md around the "second migration"
step).
