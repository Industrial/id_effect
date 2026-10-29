---
title: '0.13 follow-ups (F66) — cheaper sqlx-sqlite rows and relation pass, eager pair limit lowered: r14-f-sqlite-per-row'
slug: r14-f-sqlite-per-row
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'F66 (no scorecard row moves; data.orm is in rows_kept and must not drop): the per-value decode in crates/id_effect_sql/src/sqlite/value.rs no longer allocates for the storage class: `decode` (value.rs:189) copies `raw.type_info().name()` into a `String` for every non-NULL value (value.rs:194 `.to_owned()`) only to match on it; it becomes a `Copy` storage enum read from the borrowed name inside the match scope (no allocation on the INTEGER / REAL / BLOB paths; TEXT keeps its one `String`). `decode_rows` (value.rs:132) keeps one shared column list per result set (already so) and does no per-row name work.'
  - 'Relation pass (crates/id_effect_sql/src/repository/relations.rs): `decode` (relations.rs:511) looks the owner-key column up by name for every child row (`row.value(KEY)`, a linear name scan, crates/id_effect_sql/src/row.rs:176 via `ColumnIndex::position`) and `Key::of` (relations.rs:364) renders every owner key to a `String` (`SqlValue::render`); the column position is resolved once per result set and `Key` becomes `enum Key { Int(i64), Rendered(String) }`: integers of every width map to `Int` without formatting, a `Text` that is the canonical decimal of an `i64` also maps to `Int`, and every other value goes through `render` as today. Matching therefore stays what it is at HEAD: two keys equal after `render` today (an `I32` parent id against an `I64` child key, a `Uuid` parent id against the `Text` key SQLite returns) still match. `attach` (relations.rs:535) keeps its semantics (children cloned only when parents share a key).'
  - 'Gate limit: the `orm/eager_100x10` over `orm/n_plus_one_100x10` pair in docs/benchmarks/gates.toml (gates.toml:137-142, limit 0.45 since the owner decision of 2026-10-24) is lowered to the measured ratio plus headroom: the largest of three same-tree runs plus 0.03, rounded up to 0.01, and at most 0.30 (release spec `bench_gates.eager`; README "Breadth gates (0.13)", docs/benchmarks/README.md:378-440, measured 0.313-0.416 at 0.13 with a `flat` floor of 0.26-0.29). If the measured limit would exceed 0.30, the leaf stops and reports the numbers to the owner instead of writing a limit above 0.30. The gate comment, the README rows (README.md:32 and the pair row at :416) and the bench doc comment (crates/id_effect_sql/benches/repository_eager.rs:15, which still says 0.25) state the new limit and the run it comes from.'
  - 'Tests: the existing SQLite suites pass unchanged (crates/id_effect_sql/tests/sqlite.rs, crates/id_effect_sql/tests/relations.rs shared cases `eager_load_issues_one_query_per_relation`, `nested_eager_load_three_levels`, `m19_eager_loaded_children_scoped_to_tenant`, crates/id_effect_sql/tests/pagination.rs `keyset_with_eager_relations`, the conformance suite); new unit tests in value.rs `every_storage_class_decodes_to_its_portable_value` and `declared_bool_rejects_integers_other_than_0_and_1`; new unit tests in relations.rs `integer_keys_of_every_width_group_together`, `text_and_uuid_keys_group_as_before`, `integer_text_and_integer_keys_still_match`, `owner_key_column_is_resolved_once_per_result_set`.'
  - 'Book: none needed (no API change); docs/benchmarks/README.md new subsection "F66 (0.14)" under "Breadth gates (0.13)" with machine, tree, the three runs (eager, N+1, flat, ratios) and the base comparison against v0.13.0. Example: none.'
  - 'Tier: integration (id_effect_sql). Secure by default: no new surface; decoding errors keep naming the column (`SqlError::from_value_error`, value.rs:151).'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_sql --all-features --summary-only --fail-under-regions 96`'
  - 'Gate: `cargo nextest run -p id_effect_sql --all-features`; `cargo clippy -p id_effect_sql --all-targets --all-features -- -D warnings`; bench compare against v0.13.0 on one machine (not while another bench runs): `bash scripts/bench-run.sh --ref v0.13.0 --baseline r14sq-base --only id_effect_sql:repository_eager && bash scripts/bench-run.sh --baseline r14sq-head --only id_effect_sql:repository_eager && bash scripts/bench-export.sh r14sq-base .tmp/r14-f-sqlite-per-row/base.json && bash scripts/bench-export.sh r14sq-head .tmp/r14-f-sqlite-per-row/head.json && bash scripts/bench-compare.sh --gates .tmp/r14-f-sqlite-per-row/orm-gates.toml --enforce pair,budget .tmp/r14-f-sqlite-per-row/base.json .tmp/r14-f-sqlite-per-row/head.json`, where orm-gates.toml holds the header of docs/benchmarks/gates.toml, the new pair gate exactly as written there, and `budget` gates (limit 1.0, noise 0.05) for `orm/eager_100x10` and `orm/flat_100x10` against the v0.13.0 base (the per-row work must not get slower); the head run is repeated three times for the limit.'
  - 'Owns: crates/id_effect_sql/src/sqlite/value.rs, crates/id_effect_sql/src/repository/relations.rs (`Key`, `decode`, `attach` and the key-column lookup only), crates/id_effect_sql/benches/repository_eager.rs (module doc comment only), docs/benchmarks/gates.toml (the `orm/eager_100x10` gate block and its comment only), docs/benchmarks/README.md (line 32 row, the pair row of the "Breadth gates (0.13)" table and the new "F66 (0.14)" subsection only), CHANGELOG.md (append-only), .tmp/r14-f-sqlite-per-row/** (scratch)'
---

# r14-f-sqlite-per-row

Leaf of `.maestro/specs/releases/0.14.0.md`, section "0.13 follow-ups". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 0 head. Blocked by: nothing (release/0.14.0 at v0.13.0). Blocks: r14-e-benches (wave 5, keeps the
F66 pair at this leaf's limit), r14-x-kpis (bench compare against v0.13.0). Shares no file with r14-s-core-errors (the other wave-0
code leaf); both run benches, so they must not bench at the same time on the shared machine.

Defaults the owner may overturn: (1) the target is "≤ 0.30", not 0.25: README.md:439-440 shows that
two statements over 1,100 rows on raw sqlx already cost about a quarter of the N+1 loop, so 0.25 is
below the driver floor and no change in these files can reach it; the leaf records how close it got.
(2) The bench itself (100 x 10, SQLite in memory) is not changed, so the ratio stays comparable with
0.13. Non-goals: replacing sqlx-sqlite, a column-index cache in `FromRow` derives (per-column name
lookups in `C::from_row`), the PostgreSQL and MySQL decoders.
