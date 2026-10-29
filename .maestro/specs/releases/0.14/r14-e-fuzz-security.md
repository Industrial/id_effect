---
title: 'Proof — Fuzz targets for the 0.14 decoders, compile-fail cases for uploads and profiling, the 0.14 security surface: r14-e-fuzz-security'
slug: r14-e-fuzz-security
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '≥ 5 new cargo-fuzz targets under fuzz/fuzz_targets/ (23 today, docs/security/kpis-0.13.json `fuzz_targets` = 23 → ≥ 28), each a `[[bin]]` of fuzz/Cargo.toml with a seed corpus under fuzz/corpus/<target>/: `web_multipart_upload` (`upload::read_parts(policy, content_type, body)` of r14-s-web-uploads, web.storage#6), `mail_inbound_mime` (`parse_mime` in crates/id_effect_mail/src/inbound/ of r14-s-mail, msg.email#6), `kafka_message_headers` (`id_effect_kafka::decode_record`, r14-s-kafka, msg.brokers#1), `amqp_properties` (`id_effect_amqp::decode_properties`, r14-s-amqp, msg.brokers#2) and `event_upcast` (`Upcasters::upcast(version, &[u8])`, r14-s-events-upcast, data.events#7); the target names are the ones the providing leaves already cite, and each of them makes its decoder a documented public fn. Each builds and runs 5 minutes in ClusterFuzzLite PR mode (.github/workflows/cflite_pr.yml, .clusterfuzzlite/) with no crash; the batch workflow (cflite_batch.yml) has no open crash at exit.'
  - 'Compile-fail security cases in tests/security_compile_fail/ui (21 cases today, kpis-0.13.json `compile_fail_cases` = 21 → ≥ 23): `r4_upload_route_without_size_cap` (an upload route of r14-s-web-uploads mounted without a byte cap does not compile; ADR 0013 R4) and `r7_profiling_endpoint_without_guard` (the profiling endpoint of r14-s-console-profiling mounted without a guard does not compile; R7), each `.stderr` with a `help:` line naming the fix; picked up by tests/security_compile_fail/tests/compile_fail.rs (it runs every `ui/*.rs` stem), with each case''s crate feature added to the suite''s Cargo.toml.'
  - '`tests/security_compile_fail/tests/surface_014.rs` re-asserts, as surface_013.rs does (its `every_security_table_test_exists`, `PENDING` at :73), that every test id in the "## Security" table of .maestro/specs/releases/0.14.0.md exists in the tree and every fuzz target it names is a `[[bin]]` with seeds; its `PENDING` list is empty at exit. surface_013.rs stays green.'
  - 'THREAT-MODEL.md gains a "Sweep surfaces (0.14)" section: multipart uploads (size, count, filename, content type), inbound mail (MIME bombs, header injection, routing spoofing), Kafka and AMQP consumers (poison messages, header size, redelivery storms), event upcasting (untrusted historic payloads), on-demand profiling and stack dumps (operator surface, data exposure), Kubernetes manifests (privileges, secrets), and Warden (agent tools reached by prompt injection, bounded by capability types); each threat mapped to its test or fuzz target, residual risks stated. docs/security/fuzzing.md lists the five new targets and their decoders.'
  - 'Tier: example (security_compile_fail, fuzz) and docs. Secure by default: this leaf is the mechanical check that every other 0.14 leaf''s rule exists (R4 for uploads, mail and broker payload caps, R7 for profiling, R1 for every new route).'
  - 'Coverage: not applicable (example-tier test crates and fuzz targets; no library source).'
  - 'Gate: `cargo +nightly-2026-07-10 fuzz build` (in fuzz/, toolchain from fuzz/rust-toolchain.toml); `TMPDIR=/data/tmp cargo +nightly-2026-07-10 fuzz run <target> -- -max_total_time=300` for each new target; `cargo nextest run -p security_compile_fail`; `bash scripts/security-kpis.sh --self-test`'
  - 'Owns: `fuzz/**` (new targets, corpora, `[[bin]]` entries; `.clusterfuzzlite/**` only if a target needs a build dependency there, e.g. cmake for rdkafka), `tests/security_compile_fail/**`, `THREAT-MODEL.md` (the 0.14 section), `docs/security/fuzzing.md`'
---

# r14-e-fuzz-security

Leaf of `.maestro/specs/releases/0.14.0.md`, section "Proof". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 4. Blocked by: r14-s-kafka, r14-s-amqp and r14-s-events-upcast (W1), r14-s-web-uploads and
r14-s-mail (W2), r14-s-console-profiling (W3); each exposes the decoder or the typestate its case
needs. Runs beside r14-p-ledger, r14-p-relay and r14-w-warden-mesh (disjoint files). Feeds r14-x-kpis.

Defaults the owner may overturn: the Kafka target fuzzes `decode_record` without linking
librdkafka (fuzz/Cargo.toml takes id_effect_kafka with default features off, if r14-s-kafka keeps
the decoder outside the `rdkafka`-gated code); if it cannot, the ClusterFuzzLite image gains cmake
and the target links rdkafka's `cmake-build`.
The compile-fail case names (ADR 0013 R4 bounded input, R7 operator surfaces) are the ones
r14-s-web-uploads and r14-s-console-profiling already cite.
