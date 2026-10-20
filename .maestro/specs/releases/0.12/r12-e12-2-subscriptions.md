---
title: 'E12.2 — Subscriptions over id_effect_events with resumable cursors: r12-e12-2-subscriptions'
slug: r12-e12-2-subscriptions
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'A Subscription procedure can be backed by an id_effect_events EventLog (crates/id_effect_events/src/subscription.rs:39 read_all(after, limit)): the cursor is the global position, the stream replays from the cursor and then tails live events without a gap at the hand-over; an optional projection filter narrows the stream.'
  - 'Interrupting the subscriber releases the reader (no leaked polling fiber); the read batch size and the poll interval are bounded config.'
  - 'Tests `subscription_replays_from_cursor_then_tails`, `subscription_resume_has_no_gap_or_duplicate`, `subscription_interrupt_releases_reader` (memory EventLog), and the same three against Postgres under ID_EFFECT_REQUIRE_PG=1 (own container, never the ambient DATABASE_URL); book part6/ch33-08-subscriptions.md.'
  - 'Gate: `cargo nextest run -p id_effect_rpc --features events`; `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_rpc --features events --test subscription_events`'
  - 'Owns: `crates/id_effect_rpc/src/subscription/{mod.rs,events.rs}`, `crates/id_effect_rpc/tests/subscription_events.rs`, `crates/id_effect/book/src/part6/ch33-08-subscriptions.md`'
---

# r12-e12-2-subscriptions

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.2 — Streams and realtime". The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: r12-e12-1-kinds. r12-e12-2-channels builds topic subscriptions on it; the Relay
uses it for room history.
