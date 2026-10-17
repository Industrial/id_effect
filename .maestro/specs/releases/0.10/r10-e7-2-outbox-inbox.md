---
title: 'E7.2 — Brokers, `id_effect_messaging` + Redis Streams: r10-e7-2-outbox-inbox'
slug: r10-e7-2-outbox-inbox
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`Outbox::stage(&SqlTransaction, msg)` (#7); relay claims rows with `SKIP LOCKED`, publishes, marks sent, retries with `Schedule`; `Inbox::once(&SqlTransaction, consumer, message_id, f)` dedups inside the handler''s transaction (#8). Tests `outbox_row_rolls_back_with_tx`, `relay_publishes_each_row_once_across_two_relays`, `inbox_skips_duplicate_delivery`.'
  - 'Gate: `<pg> -p id_effect_messaging --all-features`'
  - 'Owns: `crates/id_effect_messaging/src/{outbox.rs,inbox.rs,relay.rs}`, `crates/id_effect_messaging/migrations/`'
---

# r10-e7-2-outbox-inbox

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E7.2 — Brokers, `id_effect_messaging` + Redis Streams (L, wave 2) · msg.brokers #3 #5 #6 #7 (+#4 #8 kept)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
