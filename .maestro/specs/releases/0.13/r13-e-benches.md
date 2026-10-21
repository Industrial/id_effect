---
title: 'Proof — Benches and gates for eager loading, i18n formatting and UI rendering: r13-e-benches'
slug: r13-e-benches
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'Benches `orm/eager_100x10` and `orm/n_plus_one_100x10` (100 parents × 10 children, SQLite in-memory, same run) in crates/id_effect_sql/benches/repository_eager.rs; `pair` gate `orm/eager_100x10 / orm/n_plus_one_100x10 ≤ 0.25` in docs/benchmarks/gates.toml.'
  - '`i18n/format_plural` (crates/id_effect_i18n/benches/format.rs) and `ui/render_list_50` (crates/id_effect_axum/benches/ui_render.rs) as `report` gates; after the first measurement on the bench runner, a `budget` gate is written for each at measured p50 × 1.5 (D15), and the owner confirms the numbers at exit.'
  - 'Every 0.12 gate (secure-stack delta ≤ 5 µs, unary RPC delta ≤ 3 µs, batch pair ≤ 0.20, h3/h2 pair) unchanged and green; the streaming group compared against v0.12 for r13-f-stream-chunking; results stored as docs/benchmarks/v0.13.json; README updated.'
  - 'Tier: integration (bench targets in three crates) and docs. Secure by default: not applicable (no surface).'
  - 'Gate: `bash scripts/bench-run.sh && bash scripts/bench-compare.sh docs/benchmarks/v0.12.json <new>` (no budget/delta/pair failure)'
  - 'Owns: `crates/id_effect_sql/benches/repository_eager.rs`, `crates/id_effect_i18n/benches/format.rs`, `crates/id_effect_axum/benches/ui_render.rs` (and their `[[bench]]` manifest entries), `docs/benchmarks/{gates.toml,README.md,v0.13.json}`, `scripts/bench-run.sh`'
---

# r13-e-benches

Leaf of `.maestro/specs/releases/0.13.0.md`, section "Proof". The release spec's row is binding; this
file exists so Maestro can track the leaf as its own task.

Wave 4. Blocked by: r13-e3-3-relations, r13-e3-3-pagination, r13-e9-4-i18n-core, r13-e9-5-views,
r13-f-stream-chunking. Runs on an idle machine. The `[[bench]]` lines in the sql, i18n and axum
manifests are the only manifest edits; no other wave-4 leaf touches those manifests.
