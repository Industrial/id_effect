---
title: 'E3.3 part 2 — Keyset (cursor) pagination for repositories: r13-e3-3-pagination'
slug: r13-e3-3-pagination
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'data.orm #2 strengthened (E3.3 "pagination"): `Repository::find_after(PageCursor)` / `find_before` returning `Paged<T> { items, next, prev }` over the primary key plus declared sort columns; stable under concurrent inserts; the offset `Page` (`find_page`) is kept.'
  - 'D11 cursor format: opaque base64url of the sort-key tuple and a fingerprint of the filter set; ≤ 1 KiB; decoded through a bounded Schema; not signed because every query re-applies tenant and access scope.'
  - 'Secure by default (ADR 0013 R4, R1): oversize cursor rejected; a cursor whose fingerprint differs from the request filters refused; page size default 50, max 500 (configurable, finite). Tests `r4_page_cursor_over_limit_rejected`, `cursor_with_other_filter_refused`, `r4_page_size_capped`, `tampered_cursor_stays_in_scope`.'
  - 'Functional tests `keyset_stable_under_concurrent_inserts`, `keyset_prev_and_next_round_trip`, `keyset_with_eager_relations`; `PageCursor::decode` is public for the fuzz target `sql_page_cursor`.'
  - 'Book `part6/ch28-07-pagination.md`.'
  - 'Tier: integration. Coverage: `cargo llvm-cov nextest -p id_effect_sql --features repository,sqlite --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_sql --features repository,sqlite --test pagination`'
  - 'Owns: `crates/id_effect_sql/src/repository/page.rs`, `crates/id_effect_sql/tests/pagination.rs`, `crates/id_effect/book/src/part6/ch28-07-pagination.md`'
---

# r13-e3-3-pagination

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E3.3 part 2". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r13-e3-3-relations. Runs beside r13-e3-3-hooks (separate files; `repository/mod.rs`
module lines append-only). Blocks r13-e9-3-admin-edit and r13-e-fuzz-security.
