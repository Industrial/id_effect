---
title: 'T12 carry (F27, F56) — generated Rust clients batch same-tick calls and attach headers: r13-f-client-batching'
slug: r13-f-client-batching
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'F27: apply `.maestro/specs/releases/0.13/f27-client-batching.patch` (written against a344bd9e; touches CHANGELOG.md, crates/id_effect_rpc/src/client/http.rs, crates/id_effect_rpc/src/service.rs, crates/id_effect_rpc/tests/service_macro.rs), then shrink it with `Effect::any_env` (r13-f-any-env): the per-caller-env batch resolver table goes away; calls a `#[rpc::service]` client makes over `HttpTransport` in one tick go out as one batch (≤ 50 calls / 1 MiB, same rules as `RpcClient::call`), each with its service version; a lone call is a plain envelope.'
  - 'F56: `HttpTransport::with_header(name, value)` and `with_bearer(secret)`; names and values validated (typed error otherwise); `x-rpc` and `content-type` are reserved and cannot be overridden; bearer tokens are `Secret` and redacted in `Debug`.'
  - 'Secure by default (ADR 0013 R2, R4, R9): header validation and the reserved-header rule keep the 0.12 CSRF rule (`x-rpc: 1` on every POST) intact. Tests `http_transport_rejects_invalid_header`, `http_transport_cannot_override_rpc_header`, `http_transport_bearer_redacted_in_debug`.'
  - 'Functional tests `generated_client_batches_same_tick_calls`, `generated_client_lone_call_is_plain_envelope`, `http_transport_attaches_bearer`; existing batching and service_macro tests stay green.'
  - 'Tier: integration (id_effect_rpc, platform tier since 0.12). Coverage: `cargo llvm-cov nextest -p id_effect_rpc --all-features --summary-only --fail-under-regions 96`; new code ≥ 96 % regions.'
  - 'Gate: `cargo nextest run -p id_effect_rpc --all-features`; `cargo nextest run -p id_effect_rpc`; `cargo clippy -p id_effect_rpc --no-default-features --features client -- -D warnings`'
  - 'Owns: `crates/id_effect_rpc/src/{service.rs,client/http.rs}`, `crates/id_effect_rpc/tests/service_macro.rs` (new tests only), its CHANGELOG hunk'
---

# r13-f-client-batching

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T12 carry" (0.12 overlay F27 and F56, from the
blocked 0.12 leaf r12-f-client-batching). The release spec's row is binding; this file exists so
Maestro can track the leaf as its own task.

Wave 1. Blocked by: r13-f-any-env. Blocks r13-f-relay-host. If the patch no longer applies on the
v0.12.0 tag, re-derive it from the 0.12 leaf spec `.maestro/specs/releases/0.12/r12-f-client-batching.md`.
