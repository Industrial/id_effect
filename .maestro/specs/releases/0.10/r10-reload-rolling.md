---
title: 'RELOAD — config hot reload + rolling restart: r10-reload-rolling'
slug: r10-reload-rolling
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Kamal template sets the proxy healthcheck to `/ready` and a drain timeout ≥ the server''s shutdown grace; test asserts the rendered values; chapter documents the rolling sequence, citing `on_sigterm_ready_turns_503_while_an_in_flight_request_completes` (`crates/id_effect_axum/tests/health.rs:216`) as the evidence (#4).'
  - 'Gate: `cargo nextest run -p id_effect_tool`; `cargo test --doc -p book_snippets`'
  - 'Owns: `crates/id_effect_tool/templates/service/config/deploy.yml.template`, `crates/id_effect_tool/tests/bin.rs` (one test), `book/src/part6/ch34-00-dx-ship.md` (rolling-drain section)'
---

# r10-reload-rolling

Leaf of `.maestro/specs/releases/0.10.0.md`, section "RELOAD — config hot reload + rolling restart (M, wave 1) · fault.reload #4 #5". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
