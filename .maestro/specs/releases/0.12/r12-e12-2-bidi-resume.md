---
title: 'E12.2 — Two-way streams with backpressure and resume cursors: r12-e12-2-bidi-resume'
slug: r12-e12-2-bidi-resume
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'web.rpc#4 (bidirectional): Duplex<In, Out, E> over WebSocket with credit-based backpressure in both directions; memory per stream bounded by the credit window.'
  - 'Resume: every server-to-client message carries a cursor; after a reconnect the client sends session id + last cursor and the server replays from a bounded per-session buffer (default 1,024 messages / 1 MiB / 30 s, config); a cursor older than the buffer yields the typed ResumeError::Expired so the client restarts the stream; subscriptions resume from the event store instead (r12-e12-2-subscriptions).'
  - 'Security (ADR 0013 R1): session ids are 128-bit random and bound to the authenticated principal; resuming under another principal is refused and counted.'
  - 'Tests `bidi_backpressure_bounds_memory`, `resume_after_disconnect_no_gaps_no_duplicates` (proptest over random disconnect points, 1,000 cases, TestClock), `resume_after_buffer_overrun_is_expired`, `resume_under_another_principal_refused`, `session_buffer_released_after_retention`; `resume::decode_cursor` is public for the fuzz target; book part6/ch33-07-two-way-streams.md.'
  - 'Gate: `PROPTEST_CASES=1000 cargo nextest run -p id_effect_rpc --all-features --test resume_property`; `cargo nextest run -p id_effect_rpc --all-features`'
  - 'Owns: `crates/id_effect_rpc/src/{duplex.rs,resume.rs}`, `crates/id_effect_rpc/src/transport/ws.rs` (resume handshake, after r12-e12-2-rpc-ws-sse), `crates/id_effect_rpc/tests/{bidi.rs,resume_property.rs}`, `crates/id_effect/book/src/part6/ch33-07-two-way-streams.md`'
---

# r12-e12-2-bidi-resume

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.2 — Streams and realtime". The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 4, critical path. Blocked by: r12-e12-2-rpc-ws-sse (owns `transport/ws.rs` before it).
r12-e12-3-webtransport, r12-e12-4-gen-ts and r12-e12-5-fuzz-security wait for it. The resume
property test is the exit gate's "0 gaps, 0 duplicates in 1,000 randomized runs".
