---
title: 'E12.1 — Six procedure kinds, typed error unions, cancellation: r12-e12-1-kinds'
slug: r12-e12-1-kinds
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'Six kinds per the design note: query (GET /rpc/<tag>?input=<json> with ETag and If-None-Match → 304, or POST; retry-safe), mutation (POST; optional Idempotency-Key replayed from an IdempotencyStore trait with a bounded in-memory store, TTL 24 h, 100,000 keys, D14), server stream (NDJSON response), client stream (chunked NDJSON request with item and byte caps), and the runtime types Duplex and Subscription with an in-process transport (their wire mappings come in waves 3–4).'
  - 'Typed errors: the handler E becomes a tagged union {"_tag": ...} in the contract and on the wire; defects (panics, Die) become InternalError { id } carrying only the correlation id; no message or backtrace reaches the client (ADR 0013 R3).'
  - 'Cancellation: a client disconnect or cancel interrupts the handler fiber and its scope finalizers run (the 0.11 inline executor DropPolicy::Interrupt path); a per-call deadline header is capped by the server maximum.'
  - 'Browser CSRF (ADR 0013 R9): mutations require Content-Type application/json (or the postcard type) and the x-rpc header; the 0.11 CSRF layer applies to cookie sessions.'
  - 'Tests `query_get_returns_etag_and_304`, `mutation_idempotency_key_replays_result`, `client_stream_over_cap_rejected`, `typed_error_reaches_client_as_tagged_union`, `defect_becomes_internal_error_without_message`, `client_disconnect_interrupts_handler_and_runs_finalizers`, `deadline_capped_by_server_max`, `r9_rpc_mutation_without_custom_header_refused`, `r9_rpc_form_content_type_refused`; book part6/ch33-01-procedures.md.'
  - 'Gate: `cargo nextest run -p id_effect_rpc --all-features`; `cargo clippy -p id_effect_rpc --all-targets --all-features -- -D warnings`'
  - 'Owns: `crates/id_effect_rpc/src/{kinds.rs,handler.rs,cancel.rs,error_union.rs,idempotency.rs}`, `crates/id_effect_rpc/src/server/{mod.rs,dispatch.rs,http.rs}`, `crates/id_effect_rpc/tests/{kinds_http.rs,cancellation.rs,errors.rs}`, `crates/id_effect/book/src/part6/ch33-01-procedures.md`'
---

# r12-e12-1-kinds

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.1 — RPC". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r12-e12-1-promote and the review of `r12-rpc-design.md`. On the critical path:
batching, postcard, macro, subscriptions and rpc-ws-sse start after it.
