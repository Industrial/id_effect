---
title: 'Follow-up — unary RPC under the D5 budget, TCP_NODELAY on the host: r12-f-unary-perf'
slug: r12-f-unary-perf
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'D5: `rpc/unary_json` − `rpc/axum_json_handler` ≤ 3,000 ns on the r12-e12-5-benches harness (measured +3,399 ns at 9f422370, Maestro evidence evd-1792438306570-c6acf9), median of 5 runs at load < 4, recorded in docs/benchmarks/v0.12.json; then the D5 `[[gate]]` table in docs/benchmarks/gates.toml (as docs/benchmarks/README.md "Unary budget (D5)" shows) is enabled. The budget itself is not raised.'
  - 'Method: profile the unary path (perf / cargo-flamegraph or criterion + counting allocations) and remove per-call work that is not security-relevant — double JSON (de)serialisation, per-call allocations, repeated header parsing, boxed futures, guard-chain clones. No security check is removed or weakened (x-rpc, Content-Type, guards, deadline clamp, InternalError redaction, idempotency); secure-stack gate stays green (≤ +5 µs).'
  - 'F48: the axum host listener and `serve_router*` set `TCP_NODELAY` on accepted TCP connections (WebSocket p99 41-42 ms → ~1 ms measured by benches); test asserts nodelay on an accepted socket.'
  - 'Gate: `cargo nextest run -p id_effect_rpc -p id_effect_axum --all-features`; clippy -D warnings; bench-run + bench-compare for rpc and axum gates; coverage of changed code ≥ 96% regions.'
  - 'Owns: `crates/id_effect_rpc/src/{server/**,handler.rs,registry.rs,codec/json.rs,wire.rs}`, `crates/id_effect_axum/src/server/{connection.rs,lifecycle.rs,bootstrap.rs}` (nodelay only), `docs/benchmarks/{gates.toml,v0.12.json,README.md}`, CHANGELOG own hunk.'
---

# r12-f-unary-perf

Coordinator follow-up leaf of `.maestro/specs/releases/0.12.0.md` (E12.5 / D5). The benches leaf measured
+3,399 ns against the 3 µs unary budget and stopped without loosening it (release spec Open question 2).
Coordinator decision (as in 0.11, where the secure stack was trimmed from 43.8 µs to 3.5 µs): one
performance pass; the budget stands. If the pass cannot reach 3 µs without weakening a security check,
the leaf reports the floor with a profile, and the budget question goes back to the owner at exit.
