---
title: 'E12.5 — T12 bench gates: unary budget, batch, 10,000 streams, HTTP/3 under loss: r12-e12-5-benches'
slug: r12-e12-5-benches
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'Criterion benches in crates/id_effect_rpc/benches/gate_rpc.rs: rpc/unary_json (a #[rpc::service] query, JSON, small struct, through the in-process tower path as the 0.11 gate_request bench), rpc/axum_json_handler (a raw axum handler doing the same serde JSON decode and encode of the same struct; the baseline), rpc/unary_postcard, rpc/batch_50 and rpc/sequential_50 (50 calls over loopback TCP with keep-alive, sequential awaits).'
  - 'D5: a delta gate rpc/unary_json − rpc/axum_json_handler ≤ 3,000 ns (same run and runner; ratio printed for information) is written into docs/benchmarks/gates.toml only after the first measurement is recorded in the leaf evidence; if the measurement exceeds 3 µs the leaf stops and asks the owner (release spec Open question 2) instead of loosening it.'
  - 'Pair gate rpc/batch_50 / rpc/sequential_50 ≤ 0.20 (≥ 5× faster); the 0.11 secure-stack delta gate (≤ 5 µs) is unchanged.'
  - 'Soak: examples/streams_soak.rs opens 10,000 concurrent WebSocket server streams on one node with heartbeats and samples RSS; pass when RSS at minute 10 is ≤ 1.05 × RSS at minute 1 and every stream received every heartbeat; a 60 s variant runs in PR CI, the 10-minute run in bench.yml.'
  - 'D9: benches/h3_loss.rs measures p99 of 64 multiplexed 16 KiB responses over one connection, HTTP/2 over TLS vs HTTP/3, with tc netem loss 1% delay 5ms on loopback (bench.yml job with sudo); pair gate h3_p99 / h2_p99 ≤ 1.00 (noise 0.05); results published in docs/benchmarks/v0.12.json and README.'
  - 'Gate: `bash scripts/bench-run.sh` then `bash scripts/bench-compare.sh docs/benchmarks/v0.11.json <new>` (no budget/delta/pair FAIL); `cargo run --release -p id_effect_rpc --example streams_soak -- --streams 10000 --minutes 10`; `bash scripts/check-workflows.sh`'
  - 'Owns: `crates/id_effect_rpc/benches/{gate_rpc.rs,h3_loss.rs}`, `crates/id_effect_rpc/examples/streams_soak.rs`, `docs/benchmarks/{gates.toml,README.md,v0.12.json}`, `scripts/bench-run.sh`, `.github/workflows/bench.yml`'
---

# r12-e12-5-benches

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.5 — Consumers and proof". The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 4. Blocked by: r12-e12-1-batching, r12-e12-2-rpc-ws-sse, r12-e12-3-h3-server,
r12-e12-3-h3-client. Measure on an idle machine (0.10 lesson: a loaded machine skews benches). Why the
unary gate is absolute: since 0.11 runs request fibers inline (bf7be935) a bare axum request costs
0.90 µs, so a 1.10× ratio would leave about 90 ns for the whole RPC layer and would mostly measure the
denominator. Owner step D16 (netem on the bench runner) applies.
