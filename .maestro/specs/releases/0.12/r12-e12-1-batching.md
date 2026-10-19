---
title: 'E12.1 — Batching with RequestCache deduplication and per-call limits: r12-e12-1-batching'
slug: r12-e12-1-batching
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'POST /rpc with a JSON array body runs a batch: ids echoed, results in request order, each call authorized, rate-limited, traced and failing on its own; bounded parallelism through the compute admission (crates/id_effect/src/compute/admission.rs), an ordered flag runs calls in sequence.'
  - 'One id_effect::request::RequestCache (crates/id_effect/src/request.rs:309) is scoped per batch, so duplicate loads through a RequestResolver are merged (the DataLoader equivalent).'
  - 'Finite defaults (ADR 0013 R4): at most 50 calls and 1 MiB per batch (config), 413 Problem otherwise; the rate limiter is charged N tokens for a batch of N (id_effect_axum middleware/rate_limit.rs gains a per-request charge).'
  - 'The Rust client batches calls made in the same tick into one request (bounded window, opt-out per call).'
  - 'Tests `batch_results_in_request_order_each_fails_alone`, `batch_dedups_duplicate_loads_through_request_cache`, `ordered_batch_runs_sequentially`, `r4_batch_over_limit_rejected_413`, `r4_batch_charges_n_rate_limit_tokens`, `client_batches_same_tick_calls`; `wire::decode_batch` is public for the fuzz target; book part6/ch33-02-batching.md.'
  - 'Gate: `cargo nextest run -p id_effect_rpc -p id_effect_axum --all-features`'
  - 'Owns: `crates/id_effect_rpc/src/{batch.rs,client/http.rs}`, `crates/id_effect_rpc/src/server/http.rs` (batch branch, after r12-e12-1-kinds), `crates/id_effect_axum/src/middleware/rate_limit.rs`, `crates/id_effect_rpc/tests/batching.rs`, `crates/id_effect/book/src/part6/ch33-02-batching.md`'
---

# r12-e12-1-batching

Leaf of `.maestro/specs/releases/0.12.0.md`, section "E12.1 — RPC". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: r12-e12-1-kinds. Runs beside r12-e12-1-postcard, r12-e12-1-macro,
r12-e12-2-subscriptions, r12-e12-2-rpc-ws-sse and r12-e12-3-h3-server (disjoint files).
