---
title: 'T0 — MIGRATION-0.11 and CHANGELOG (exit): r11-t0-migration-guide'
slug: r11-t0-migration-guide
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'docs/MIGRATION-0.11.md has one section per break reported by `cargo semver-checks` against 0.10.1, each with its shim or replacement (L9): typed routes need .public()/.guard() (shim Router::route_unchecked); query/execute take Sql (escape hatch query_raw + RawSqlCapability); id_effect_process Next/Info/SendError become #[non_exhaustive] with new variants; RedactionPolicy::new → RedactionPolicy::empty; Secret<T> requires Zeroize and compares in constant time; id_effect_fp::stm → id_effect::stm; HttpClient SSRF guard on and redirects off by default; plaintext TCP behind feature insecure-plaintext-tcp; masked CSRF token format; mesh protocol version bump (older nodes refused).'
  - 'CHANGELOG 0.11.0 has a Scorecard section (index deltas from REPORT-0.11.0.md) and a Security section (findings closed with their regression tests, and every new dangerous_*/insecure_* opt-out).'
  - 'Gate: `cargo semver-checks` output is a subset of the MIGRATION-0.11 list (the comparison is attached to the leaf evidence)'
  - 'Owns: `docs/MIGRATION-0.11.md`, `CHANGELOG.md`'
---

# r11-t0-migration-guide

Leaf of `.maestro/specs/releases/0.11.0.md`, section "§7.0b and T0" (exit). The release spec's row
is binding; this file exists so Maestro can track the leaf as its own task.

Exit wave. Blocked by: every leaf with a semver break (each drafts its own section first).
