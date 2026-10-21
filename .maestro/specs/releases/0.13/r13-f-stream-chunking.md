---
title: 'T12 carry (F57) — live streams deliver partial chunks instead of stalling: r13-f-stream-chunking'
slug: r13-f-stream-chunking
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'F57: a pull on a stream whose upstream would wait returns the items already available (between 1 and the requested size, `DEFAULT_CHUNK_SIZE = 64` at crates/id_effect/src/streaming/stream.rs:28) instead of waiting for a full chunk; `run_for_each` / `run_for_each_effect` (`:728`) on a live client stream sees each item as it arrives; bounded and pure streams keep full chunks.'
  - 'Change limited to the pull path (stream.rs pull and pump.rs feed); no public signature change.'
  - 'Tests `live_stream_delivers_partial_chunk`, `run_for_each_live_no_stall` (TestClock), `bounded_stream_chunking_unchanged`, `unfold_effect_partial_chunks`; the 0.12 RPC client-stream test from the Relay finding passes without the workaround.'
  - 'Performance: the streaming benches are within noise of docs/benchmarks/v0.12.json (no throughput regression on bounded streams).'
  - 'Tier: core (strict bar). Secure by default: no new surface; backpressure semantics unchanged (a slow consumer still slows its producer).'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect`; `cargo nextest run -p id_effect_rpc --all-features`; `bash scripts/bench-run.sh` then `bash scripts/bench-compare.sh docs/benchmarks/v0.12.json <new>` (streaming group)'
  - 'Owns: `crates/id_effect/src/streaming/{stream.rs,pump.rs}` (pull path), `crates/id_effect/tests/stream_live_chunks.rs`'
---

# r13-f-stream-chunking

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T12 carry" (0.12 overlay F57). The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: the v0.12.0 tag. Blocks r13-e-benches. Core-tier change: the reviewer checks the
bench comparison before merge.
