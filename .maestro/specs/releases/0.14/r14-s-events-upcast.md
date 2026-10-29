---
title: 'E13.1 — Sweep — event schema versions and upcasting in the SQL journal: r14-s-events-upcast'
slug: r14-s-events-upcast
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'data.events#7 (row data.events 88 → 100). What exists: `SqlEventJournal<E>` stores each payload as JSON text (crates/id_effect_events/src/sql_journal.rs:32 `SQL_EVENT_JOURNAL_DDL`: position, stream_id, version, event_id, payload) and decodes it straight into `E` (sql_journal.rs:110 `decode_event`, used by `read` and by `read_all` at :341); `EventEnvelope` / `WireEventEnvelope` (crates/id_effect_events/src/envelope.rs:11,28) carry an `event_type` but no schema version. Nothing records which shape a stored payload has, so changing `E` breaks reading old rows. Built: new crates/id_effect_events/src/upcast.rs with `Upcasters` (`Upcasters::new(current: u32)`, `.step(from, |serde_json::Value| -> Result<Value, UpcastError>)`, `build()` rejecting a gap or a duplicate in the chain `1 .. current`), and `SqlEventJournal::with_upcasters(Upcasters)`: appends record `schema_version = current`; reads (`read`, `read_all`, and so `CatchUpSubscription`) run the steps from the stored version to `current` on the JSON before decoding `E`; a row newer than `current` fails `EventStoreError::Upcast` (new variant; the enum is `#[non_exhaustive]`, crates/id_effect_events/src/error.rs:9). A journal without upcasters behaves as today (current = 1).'
  - 'Storage: `SQL_EVENT_JOURNAL_DDL` gains `schema_version BIGINT NOT NULL DEFAULT 1`; `create_table` (sql_journal.rs:154) also upgrades a 0.13 table in place (PostgreSQL `ADD COLUMN IF NOT EXISTS`; SQLite probes the column and runs `ALTER TABLE .. ADD COLUMN` when missing), and the same statement is exported as `SQL_EVENT_JOURNAL_UPGRADE_0_14` for apps that run their own migrations; docs/MIGRATION-0.14.md gets the section. Existing rows read as version 1.'
  - 'Tests: unit tests in crates/id_effect_events/src/upcast.rs `steps_run_in_order_from_the_stored_version`, `build_rejects_a_gap_in_the_chain`, `build_rejects_a_duplicate_step`, `a_failing_step_names_stream_version_and_step`; crates/id_effect_events/tests/sql_journal.rs (SQLite) `v1_rows_read_as_the_current_version`, `append_records_the_current_schema_version`, `a_row_newer_than_current_fails_with_upcast`, `create_table_upgrades_a_0_13_journal`, `upcast_rejects_a_payload_over_the_cap`, `read_all_upcasts_for_catch_up_subscriptions`; crates/id_effect_events/tests/pg_events.rs `pg_v1_rows_read_as_the_current_version`, `pg_create_table_upgrades_a_0_13_journal` (container through id_effect_test, `ID_EFFECT_REQUIRE_DOCKER=1` fails rather than skips).'
  - 'Book: crates/id_effect/book/src/part5/ch23-00-events-and-projections.md section "Schema versions and upcasting". Example: crates/id_effect_events/examples/030_upcasting.rs (an `[[example]]` entry with `required-features = ["sqlite"]` in crates/id_effect_events/Cargo.toml, as 010 and 020): v1 rows written, the event type changed, read back through one upcaster.'
  - 'Tier: integration (id_effect_events, ADR 0010 Amendment 6). Secure by default (ADR 0013 R1, release Security table row "Event upcasters"): a payload longer than `max_payload_bytes` (default 1 MiB, settable on the journal) fails `EventStoreError::Upcast` before it is parsed (test `upcast_rejects_a_payload_over_the_cap`); upcaster steps are pure JSON functions (no I/O, no access to the client); a failing step names the stream, version and step, never the payload. The fuzz target `event_upcast` is written by r14-e-fuzz-security (wave 4) against `Upcasters::upcast(version, &[u8])`, which this leaf makes public for it.'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_events --all-features --summary-only --fail-under-regions 96`'
  - 'Gate: `cargo nextest run -p id_effect_events --all-features` (with `ID_EFFECT_REQUIRE_DOCKER=1` for pg_events); `cargo clippy -p id_effect_events --all-targets --all-features -- -D warnings`; `bash scripts/tier-conformance.sh id_effect_events`'
  - 'Owns: crates/id_effect_events/src/upcast.rs (new), crates/id_effect_events/src/sql_journal.rs, crates/id_effect_events/src/error.rs (new variant only), crates/id_effect_events/src/lib.rs (module and re-export lines, append-only), crates/id_effect_events/tests/sql_journal.rs, crates/id_effect_events/tests/pg_events.rs (new tests only), crates/id_effect_events/tests/events/mod.rs (shared cases, append-only), crates/id_effect_events/examples/030_upcasting.rs (new), crates/id_effect_events/Cargo.toml (`[[example]]` entry, append-only), crates/id_effect/book/src/part5/ch23-00-events-and-projections.md (new section only), docs/MIGRATION-0.14.md (append-only), CHANGELOG.md (append-only)'
---

# r14-s-events-upcast

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 — Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r14-t0-skeletons (docs/MIGRATION-0.14.md headings). Blocks: r14-e-fuzz-security
(fuzz target `event_upcast`, wave 4), r14-x-level-audit. Shares no file with the other wave-1 leaves.

Defaults the owner may overturn: (1) the version is a journal column, not a field inside the JSON
payload, so payloads and projections that read them stay unchanged; the cost is the in-place table
upgrade in `create_table`. (2) One schema version per journal (per `E`, which is usually one enum of
the aggregate events); per-`event_type` versions are a non-goal because the journal table has no
`event_type` column. (3) `MemoryEventStore` stores typed values and needs no upcasting. (4) No
downcasting: a reader older than the data fails with `EventStoreError::Upcast`. Non-goals: snapshot
(`SqlSnapshotStore`) versioning, schema-registry integration, `EventEnvelope` field changes.
