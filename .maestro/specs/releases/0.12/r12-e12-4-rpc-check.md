---
title: 'E12.4 — id-effect rpc check: breaking-change detection against the previous contract: r12-e12-4-rpc-check'
slug: r12-e12-4-rpc-check
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'id-effect rpc check --contract <file> --against <git-ref:path | file> compares two contracts and classifies every change: breaking (procedure removed, kind changed, required input field added, input type narrowed, output field removed or narrowed, error variant added, access decision tightened) or compatible (procedure added, optional input field added, output field added); prints the diff with a reason per item.'
  - 'web.rpc#6: a breaking change passes only when the service version is bumped and the previous version is still served; exit 1 otherwise, exit 2 on unreadable input.'
  - 'Tests `removed_procedure_is_breaking`, `optional_field_added_is_compatible`, `required_field_added_is_breaking`, `error_variant_added_is_breaking_for_clients`, `breaking_change_with_version_bump_passes`, `against_git_ref_reads_old_contract`; book part6/ch33-12-contract-check.md.'
  - 'Gate: `cargo nextest run -p id_effect_tool --test rpc_check`; `cargo nextest run -p id_effect_tool`'
  - 'Owns: `crates/id_effect_tool/src/rpc/check.rs`, `crates/id_effect_tool/src/cli.rs` (the check arm only, after r12-e12-4-gen-ts), `crates/id_effect_tool/tests/rpc_check.rs`, `crates/id_effect/book/src/part6/ch33-12-contract-check.md`'
---

# r12-e12-4-rpc-check

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.4 — Clients and contracts". The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 6. Blocked by: r12-e12-4-gen-ts (owns `cli.rs` and `rpc/mod.rs` first). The Relay and Ledger CI
jobs run it against the last tag (r12-w7-relay, r12-w7-ledger).
