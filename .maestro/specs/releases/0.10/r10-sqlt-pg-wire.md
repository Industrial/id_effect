---
title: 'SQLT — id_effect_sql without sqlx `time`: r10-sqlt-pg-wire'
slug: r10-sqlt-pg-wire
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - '`TIMESTAMPTZ`/`TIMESTAMP`/`DATE`/`TIME` bind and decode through own sqlx `Type`/`Encode`/`Decode` impls on the binary wire format (i64 µs / i32 days since 2000-01-01) into jiff; `±infinity` and out-of-range values map to `SqlError` (no panic); the conformance suite date-time cases pass unchanged; new tests `pg_wire_datetime_round_trip_bounds`, `pg_infinity_is_an_error`.'
  - 'Gate: `<pg> -p id_effect_sql --all-features`'
  - 'Owns: `crates/id_effect_sql/src/pg/{value.rs,datetime.rs}`, `src/value/time_conv.rs` (deleted or pg-only replacement)'
---

# r10-sqlt-pg-wire

Leaf of `.maestro/specs/releases/0.10.0.md`, section "SQLT — id_effect_sql without sqlx `time` (M, wave 1) · D3". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
