---
title: 'E7.2 — Brokers, `id_effect_messaging` + Redis Streams: r10-e7-2-redis-streams'
slug: r10-e7-2-redis-streams
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`RedisStreamsBroker: MessageBroker` — `XADD`, `XREADGROUP COUNT = prefetch`, `XACK`, `XAUTOCLAIM` of idle pending entries (redelivery), DLQ stream after `max_deliveries` (#3 #5 #6). Container tests `consumer_group_ack_and_redelivery`, `poison_message_moves_to_dlq`, `outbox_relay_to_streams_end_to_end`. Chapter + example.'
  - 'Gate: `<docker> -p id_effect_redis --features streams`; `bash scripts/tier-conformance.sh id_effect_messaging`'
  - 'Owns: `crates/id_effect_redis/src/streams.rs`, `crates/id_effect_redis/Cargo.toml` (feature `streams`), `book/src/part6/ch31-02-brokers.md`, `crates/id_effect_redis/examples/030_streams.rs`'
---

# r10-e7-2-redis-streams

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E7.2 — Brokers, `id_effect_messaging` + Redis Streams (L, wave 2) · msg.brokers #3 #5 #6 #7 (+#4 #8 kept)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
