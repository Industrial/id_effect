---
title: 'E13.1 Sweep — AMQP/RabbitMQ MessageBroker in id_effect_amqp (lapin): r14-s-amqp'
slug: r14-s-amqp
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'msg.brokers#2 (D2): crates/id_effect_amqp (skeleton from r14-t0-skeletons) gives `AmqpBroker` over lapin (exact pin recorded, rustls TLS, no OpenSSL) implementing `id_effect_messaging::MessageBroker` (crates/id_effect_messaging/src/broker.rs:404) with the semantics of `RedisStreamsBroker` (crates/id_effect_redis/src/streams.rs:333): a `Topic` is a routing key on one durable direct exchange (`AmqpConfig::exchange`, default `id_effect`; exact match, no wildcard semantics); a `ConsumerGroup` is a durable quorum queue `<topic>.<group>` bound to it (a queue name over 255 bytes is `BrokerError::InvalidOptions`), declared by `subscribe` before it completes (so every later publish reaches the group); `publish` completes on the publisher confirm (persistent delivery mode); `prefetch` is `basic.qos`; ack is `basic.ack`; `Nack::Requeue` and an unsettled dropped `Delivery` requeue (`basic.nack requeue=true`, the drop through the subscription driver) and the attempt is the quorum queue `x-delivery-count` + 1; the `max_deliveries`-th failure and `Nack::DeadLetter` publish to the resolved dead-letter topic with the `DeadLetterReason`, `DEAD_LETTER_TOPIC`, `DEAD_LETTER_GROUP` and `DELIVERY_COUNT` headers (crates/id_effect_messaging/src/broker.rs:21-35), confirmed, then ack the original; the queue''s own `x-delivery-limit` is set above `max_deliveries` so RabbitMQ never drops first; `close` ends every stream; an unreachable broker is `BrokerError::Connection`. Row msg.brokers 75 → 100 together with r14-s-kafka (#1).'
  - '`StartFrom` (crates/id_effect_messaging/src/broker.rs:254) on AMQP: a new group''s queue holds the messages published after it was declared, for both `Earliest` and `Latest` (a queue cannot replay what was routed before it existed); the rustdoc of `AmqpBroker` and the chapter state this difference.'
  - 'Container: `RabbitMq` / `RabbitMqContainer` in crates/id_effect_test/src/containers/rabbitmq.rs (official `rabbitmq` image, 4.x alpine tag pinned, digest recorded, ready on "Server startup complete"; `amqp_url()`); dynamic mapped port (no `host_port`, which only r14-s-kafka adds). Tests start it through `id_effect_test::Shared`, read `ID_EFFECT_REQUIRE_DOCKER=1` and fail rather than skip; CI is the `-p id_effect_amqp` line r14-t0-skeletons adds to `containers-integration` (.github/workflows/ci.yml:347).'
  - 'Tests in crates/id_effect_amqp/tests/broker.rs (names mirror crates/id_effect_redis/tests/streams.rs:126-557): `amqp_consumer_group_ack_and_redelivery`, `amqp_poison_message_moves_to_dlq`, `amqp_max_deliveries_dead_letters_with_reason`, `amqp_unsettled_drop_redelivers`, `amqp_prefetch_bounds_in_flight`, `amqp_subscriptions_share_a_group_and_groups_fan_out`, `amqp_publish_waits_for_confirm`, `amqp_outbox_relay_end_to_end` (PgOutbox + Relay into RabbitMQ), `amqp_close_ends_subscription`, `amqp_unreachable_is_a_connection_error`, `amqp_plaintext_requires_dangerous_opt_out`, `amqp_oversize_message_is_dead_lettered`; crates/id_effect_test/tests/rabbitmq.rs `rabbitmq_container_accepts_a_connection`.'
  - 'Book: new chapter `crates/id_effect/book/src/part6/ch31-05-amqp.md` (exchange/queue mapping, quorum queues, confirms, redelivery and DLQ, the `StartFrom` difference), linked from SUMMARY.md and one row in the table of part6/ch31-00-async-messaging.md; example `crates/id_effect_amqp/examples/010_amqp.rs`; README `rust` fence compiled.'
  - 'Tier: integration (new crate, ADR 0010 Amendment 10 from r14-t0-skeletons; stays integration at exit per the release spec''s tier gate). Secure by default (ADR 0013): R8 `amqps://` by default; `amqp://` only through `AmqpConfig::dangerous_plaintext()`, recorded with `posture::record_dangerous` and listed by scripts/check-dangerous.sh; R2 the URL password is a `Secret`, redacted in `Debug`/`Display` and in connection errors; R4 bounded input: body cap (`max_message_bytes`, default 1 MiB, also sent as the channel `frame_max` bound) and header caps (64 entries / 16 KiB) checked before decode; an oversize message is dead-lettered as `rejected` without its payload. Fuzz target `amqp_properties` is r14-e-fuzz-security''s; this leaf makes the basic-properties/headers decoder a documented public fn for it (`id_effect_amqp::decode_properties`).'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_amqp --all-features --summary-only --fail-under-regions 96` (with Docker); id_effect_test coverage stays ≥ 96 %.'
  - 'Gate: `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p id_effect_amqp -p id_effect_test --all-features`; `cargo clippy -p id_effect_amqp --all-targets --all-features -- -D warnings`; `cargo deny check` (amq-protocol is BSD-2-Clause: allow it in deny.toml with a comment naming the crate, as for 0BSD/BSL-1.0); `bash scripts/check-dangerous.sh`; `bash scripts/tier-conformance.sh id_effect_amqp`'
  - 'Owns: `crates/id_effect_amqp/**`, `crates/id_effect_test/src/containers/rabbitmq.rs` (new), `crates/id_effect_test/tests/rabbitmq.rs` (new), `crates/id_effect/book/src/part6/ch31-05-amqp.md` (new); append-only: `crates/id_effect_test/src/containers/mod.rs` and `crates/id_effect_test/src/lib.rs` (the `rabbitmq` module and re-export lines), the AMQP row of `crates/id_effect/book/src/part6/ch31-00-async-messaging.md`, `crates/id_effect/book/src/SUMMARY.md`, `deny.toml` (the `BSD-2-Clause` allow line), `CHANGELOG.md`, `Cargo.lock`'
---

# r14-s-amqp

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r14-t0-skeletons (crate skeleton, Amendment 10, ci.yml `-p` line, kit feature
`amqp`). Blocks r14-e-fuzz-security (`amqp_properties`) and r14-e-benches (broker throughput). Runs
beside r14-s-kafka and r14-s-nats: never edits `crates/id_effect_test/src/containers/spec.rs`. Third in the scope-cut
table (msg.brokers would stay 88).

Defaults the owner may overturn: one direct exchange with routing key = topic (not one exchange per
topic); quorum queues (for `x-delivery-count`; classic queues do not count redeliveries); dead-lettering
done by the client with our headers, not RabbitMQ's DLX (so the DLQ message is the same shape as on
Redis, Kafka and NATS). Not done here: RabbitMQ streams (offset replay), AMQP 1.0, a shared broker
conformance suite.
