---
title: 'Follow-up — macro-generated Rust clients batch same-tick calls over HTTP: r12-f-client-batching'
slug: r12-f-client-batching
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'F27: calls a `#[rpc::service]` generated client makes over `HttpTransport` in the same tick go out as one batch (≤ 50 calls / 1 MiB, same rules as `RpcClient::call`), each batched call carrying its service version `v`; a call alone in its tick is still a plain envelope.'
  - '`RpcTransport` gains an effect-returning unary hook with a default (existing transports unchanged); `HttpTransport` overrides it so `call_unary` issues the request from the caller fiber and joins the batch window.'
  - '`client/http.rs` gains a JSON-value batched call carrying `v`; the batch wire is unchanged (WireCall already has `v`).'
  - 'Tests: `generated_client_batches_same_tick_calls` (two calls in one tick → one request, both answered, versions sent), `generated_client_lone_call_is_plain_envelope`; existing batching and service_macro tests stay green.'
  - 'Gate: `cargo nextest run -p id_effect_rpc --all-features` and default; clippy -D warnings (all features, default, `--no-default-features --features client`); coverage of new code >= 96% regions.'
  - 'Owns: `crates/id_effect_rpc/src/{client/http.rs,service.rs}`, `crates/id_effect_rpc/tests/service_macro.rs` (new tests only), CHANGELOG own hunk.'
---

# r12-f-client-batching

Coordinator follow-up leaf of `.maestro/specs/releases/0.12.0.md` (E12.1), from F27 in the execution
overlay (reported by r12-e12-1-batching, sized by r12-e12-4-rpc-check: ~60 lines across two files).
