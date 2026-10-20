---
title: 'E12.5 — Fuzz targets for the new decoders and the call-surface security proof: r12-e12-5-fuzz-security'
slug: r12-e12-5-fuzz-security
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'At least 5 new cargo-fuzz targets (planned 6) with seed corpora: rpc_wire_request (JSON envelope), rpc_batch (wire::decode_batch), rpc_postcard_frame (codec::postcard::decode_frame), rpc_duplex_frame (transport::frame::decode, shared by WebSocket and WebTransport), rpc_resume_cursor (resume::decode_cursor), sse_last_event_id; each builds on the pinned nightly and runs 5 minutes in ClusterFuzzLite PR mode with no crash; the nightly batch includes them.'
  - 'Compile-fail security cases in tests/security_compile_fail: r1_rpc_procedure_without_access_decision (a #[rpc::service] method without #[public] or #[guard]) and r10_zero_rtt_on_mutation (enabling 0-RTT on a mutation does not compile).'
  - 'crates/id_effect_rpc/tests/security_surface.rs asserts that every test id in the release spec Security table exists in the workspace (fails on a missing id) and runs the cross-crate cases (Origin, CSRF header, batch limit, stream cap) against one server.'
  - 'THREAT-MODEL.md gains a call-surface section (assets, entry points: /rpc, WebSocket, SSE, HTTP/3, WebTransport, mesh RPC; threats and the test that covers each); docs/security/fuzzing.md lists the new targets.'
  - 'Gate: `cd fuzz && cargo +nightly-<pinned> fuzz build`; `cargo nextest run -p security_compile_fail`; `cargo nextest run -p id_effect_rpc --all-features --test security_surface`'
  - 'Owns: `fuzz/**`, `tests/security_compile_fail/**`, `crates/id_effect_rpc/tests/security_surface.rs`, `THREAT-MODEL.md`, `docs/security/fuzzing.md`'
---

# r12-e12-5-fuzz-security

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.5 — Consumers and proof". The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 5. Blocked by: r12-e12-1-batching, r12-e12-1-postcard, r12-e12-1-macro, r12-e12-2-bidi-resume
(the public decoders and the trybuild case). Fuzz work uses heavy temp dirs: set `TMPDIR` under
`/data`, never the 2 GB `/tmp` tmpfs.
