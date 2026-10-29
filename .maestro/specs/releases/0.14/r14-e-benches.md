---
title: 'Proof — Bench gates for 1M idle fibers and pool metrics, broker throughput and the Relay p99 reported, every 0.13 gate kept: r14-e-benches'
slug: r14-e-benches
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'conc.fibers#2 (measured): `fibers/idle_1m` spawns 1,000,000 fibers that each park on a never-completed `Deferred`, then interrupts and joins them, with `fibers/empty_iter` beside it, in crates/id_effect_tokio/benches/idle_fibers.rs; a `delta` gate over `fibers/empty_iter` at measured median × 1.5 (the D15 rule of gates.toml:144-147). conc.fibers#2 itself is met by r14-s-core-runtime (test `one_million_idle_fibers_spawn_park_and_join`, crates/id_effect/tests/idle_fibers.rs, and the README section "Idle fibers (0.14)" it owns); this gate keeps the figure from regressing, and this leaf adds only the gate row to that README section.'
  - 'conc.parallel#8 overhead: `compute/admit_metrics_on` and `compute/admit_metrics_off` (the same `admit`/release loop on the compute fabric, crates/id_effect/src/compute/admission.rs:51, with the utilisation counters of r14-s-parallel-supervision, crates/id_effect/src/compute/utilisation.rs and `Fabric::utilisation()`, on and off) in crates/id_effect/benches/compute_metrics.rs; `pair` gate `compute/admit_metrics_on / compute/admit_metrics_off ≤ 1.05`, noise 0.02 (owner-confirmed at exit).'
  - 'msg.brokers / eco.fit#6 throughput, reported: `kafka/produce_consume_1k` (crates/id_effect_kafka/benches/throughput.rs) and `amqp/publish_consume_1k` (crates/id_effect_amqp/benches/throughput.rs), each publishing 1,000 1 KiB messages through `MessageBroker` (crates/id_effect_messaging/src/broker.rs:404) and consuming them with commit/ack, against the id_effect_test containers; run by a new `brokers` job in .github/workflows/bench.yml and by `bash scripts/bench-run.sh --brokers`; messages/s recorded in docs/benchmarks/v0.14.json and the README table "Broker throughput (0.14)". Not a gates.toml entry this release: a `report` gate needs the id in the v0.13 base too (scripts/bench-compare.sh:96), so the gate is written in 0.15 on this base.'
  - 'conc.perf: the Relay load p99/p999 and throughput from r14-p-relay''s `relay_load_p99_within_budget` (CI artifact `relay-load`) are copied into the README section "Relay under load (0.14)" with the run id and commit.'
  - 'F66 kept: the `orm/eager_100x10` pair (gates.toml:137-142) keeps the limit r14-f-sqlite-per-row set (0.45 at HEAD, lowered toward 0.25 by that leaf); this leaf does not change it and the 0.14 run passes it.'
  - 'Every 0.13 gate unchanged and green against docs/benchmarks/v0.13.json: the budget, improve, delta and pair gates of gates.toml (phase0 and gate/* hot paths, coord/*, `gate/axum_request_secure_stack` ≤ 5 µs, `rpc/unary_json` ≤ 3 µs, `rpc/batch_50` pair, `h3_loss/h3_p99` pair, `i18n/format_plural`, `ui/render_list_50`) and the report rows; results stored as docs/benchmarks/v0.14.json; README "0.14 exit" section.'
  - 'Tier: integration and runtime bench targets (id_effect core, id_effect_tokio runtime, id_effect_kafka and id_effect_amqp integration) and docs; bench code only, no library source. Secure by default: not applicable (no surface).'
  - 'Coverage: not applicable (bench targets only; no library source changes, so the crates'' floors are unchanged).'
  - 'Gate: `bash scripts/bench-run.sh && bash scripts/bench-export.sh head .tmp/bench-0.14.json && bash scripts/bench-compare.sh docs/benchmarks/v0.13.json .tmp/bench-0.14.json` (no budget/improve/delta/pair failure, no missing id); `bash scripts/bench-compare.sh --self-test`; `ID_EFFECT_REQUIRE_DOCKER=1 bash scripts/bench-run.sh --brokers`; `cargo bench --no-run -p id_effect -p id_effect_tokio -p id_effect_kafka -p id_effect_amqp`'
  - 'Owns: `crates/id_effect_tokio/benches/idle_fibers.rs`, `crates/id_effect/benches/compute_metrics.rs`, `crates/id_effect_kafka/benches/throughput.rs`, `crates/id_effect_amqp/benches/throughput.rs` (and their `[[bench]]` manifest entries, the only manifest edits), `docs/benchmarks/v0.14.json`, `docs/benchmarks/gates.toml` (a new "0.14" section only; the 0.13 entries and the `orm/eager_100x10` block of r14-f-sqlite-per-row untouched), `docs/benchmarks/README.md` (sections "Broker throughput (0.14)", "Relay under load (0.14)", "0.14 exit", and the gate row in r14-s-core-runtime''s "Idle fibers (0.14)"), `scripts/bench-run.sh` (the bench list and `--brokers`), `.github/workflows/bench.yml` (job `brokers`)'
---

# r14-e-benches

Leaf of `.maestro/specs/releases/0.14.0.md`, section "Proof". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 5. Blocked by: r14-f-sqlite-per-row (F66 limit), r14-s-core-runtime (conc.fibers#2),
r14-s-kafka and r14-s-amqp (the broker crates), r14-s-parallel-supervision (pool metrics) and
r14-p-relay (the published p99). Runs on an idle machine beside r14-p-toolchain and
r14-w-warden-ops (disjoint files; the `[[bench]]` lines are the only edits to the four manifests in
wave 5). Feeds r14-x-kpis.

Defaults the owner may overturn: broker throughput is recorded, not gated, because a new id cannot
be a `report` gate without a base; the 1M-fiber gate is a D15 `delta` budget on time, with RSS per
fiber left to r14-s-core-runtime's README section (criterion measures time only); if "on and off" is
not a switch r14-s-parallel-supervision exposes, the pair compares `admit` with and without a
`utilisation()` read per iteration; the pool-metrics overhead limit
is 5 %.
