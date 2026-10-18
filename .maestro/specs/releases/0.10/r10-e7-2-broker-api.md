---
title: 'E7.2 — Brokers, `id_effect_messaging` + Redis Streams: r10-e7-2-broker-api'
slug: r10-e7-2-broker-api
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`MessageBroker` per D7 (#4); `MemoryBroker` with consumer groups, redelivery and a dead-letter topic after `max_deliveries` (#5), bounded prefetch + concurrency (#6). Tests `nack_redelivers_then_dead_letters`, `prefetch_bounds_in_flight`, `two_groups_each_get_every_message`.'
  - 'Gate: `cargo nextest run -p id_effect_messaging`'
  - 'Owns: `crates/id_effect_messaging/src/{broker.rs,delivery.rs,memory.rs}`'
---

# r10-e7-2-broker-api

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E7.2 — Brokers, `id_effect_messaging` + Redis Streams (L, wave 2) · msg.brokers #3 #5 #6 #7 (+#4 #8 kept)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
