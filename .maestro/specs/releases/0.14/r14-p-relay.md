---
title: 'E13.2 Promote + F59 — the Relay exercises server and client streams, SSE, postcard, batching, WebSocket resume and a p99 load test: r14-p-relay'
slug: r14-p-relay
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'F59 (0.14 part, carried by r13-f-relay-host and CHANGELOG.md:445-447): examples/reference/relay exercises every RPC feature it skipped in 0.12–0.13. At HEAD its service (src/api.rs:186) has unary, `Duplex` (`chat`, :202) and `#[subscription]` (`join`, `history` with an `after` cursor, :211-:220) only; mesh RPC is postcard (:232), the client surface is JSON; no SSE, no client batching assertion, no transport-level WebSocket session resume (`relay_resume_after_client_reconnect_no_gaps`, tests/chat.rs:314, is the subscription cursor, not the `resume` frame).'
  - 'Server stream: `#[server_stream] export(room)` returns the room history as `Stream<Message, RelayError, DynEnv>` (bounded by the server stream window, `RpcServer::with_stream_window`, crates/id_effect_rpc/src/server/mod.rs:190). Client stream: `#[client_stream] import(items: Stream<Outgoing, ..>)` posts a bulk of messages with one `Ack` count. Tests `relay_export_streams_room_history`, `relay_import_client_stream_posts_every_message`, `relay_import_over_window_is_backpressured` (tests/streams.rs).'
  - 'SSE: the Relay''s `RpcServer` turns on `with_sse` (server/mod.rs:157) in the route builder of src/node.rs:627; `history` and `export` are read over SSE with the Rust client (`RpcClient::sse`, crates/id_effect_rpc/src/client/sse.rs:50) and resume from `Last-Event-ID`. Tests `relay_history_over_sse`, `relay_sse_resumes_from_last_event_id`.'
  - 'Postcard: the client surface also speaks postcard: WebSocket with `WsClientConfig::postcard` (client/ws.rs:137) and HTTP with `RpcClient::call_postcard` (client/http.rs:136). Tests `relay_chat_over_postcard_websocket`, `relay_send_over_postcard_http` (same results as JSON).'
  - 'Batching: the Relay''s server sets an explicit `BatchConfig` (crates/id_effect_rpc/src/batch.rs:94, `with_batch_config` :184); a client fan-out of `send` calls in one `all(..)` goes out as one batch request (client/http.rs:4-10). Tests `relay_sends_in_one_tick_are_one_batch_request` (request count observed at the server), `relay_batch_over_cap_rejected` (R4: over `max_calls` / `max_bytes` refused with the documented status).'
  - 'WebSocket session resume: the Relay sets `RpcWsConfig::resume(ResumeConfig)` (crates/id_effect_rpc/src/transport/ws.rs:257) explicitly on the one config both transports share (src/node.rs:633-634). Tests `relay_chat_resumes_websocket_session_after_drop` (raw `resume` frames as crates/id_effect_rpc/tests/resume_property.rs does, since the Rust `WsClient` sends none), `relay_typescript_client_resumes_session` (the generated client under Deno; its runtime resumes, crates/id_effect_tool/src/rpc/ts/runtime.ts:17), `relay_resume_under_another_principal_refused` and `relay_resume_stats_count_refusals` (`resume_stats`, ws.rs:268).'
  - 'conc.perf (documented → production; score stays 88, #5 is an external listing): tests/load.rs `relay_load_p99_within_budget` drives three nodes with N WebSocket clients × M messages (defaults 200 × 50) and measures send → delivery latency; it writes p50/p99/p999 and throughput as JSON to `$RELAY_LOAD_OUT` and fails when p99 exceeds `RELAY_LOAD_P99_BUDGET_MS` (default 250 ms on the GitHub runner, owner-confirmed at exit). The `reference-relay` job runs it in the release profile as its own step, uploads the JSON as artifact `relay-load` and writes the figures to `$GITHUB_STEP_SUMMARY`; r14-e-benches copies them into docs/benchmarks/README.md.'
  - 'The contract change is additive: rpc.contract.json and the generated TypeScript client (web/src/rpc) regenerated; the `id-effect rpc check --against <last tag>` step of `reference-relay` (ci.yml:556-570) passes against v0.13.0; every existing `relay_*` test (tests/{binary,browser,chat,contract,edges,fallback,host,manifest,posture}.rs) passes unchanged; `relay_depends_on_kit_only` stays green (every feature used comes through kit `rpc`/`rpc-ws`, crates/id_effect_kit/Cargo.toml:73-76).'
  - 'Book: no new chapter (web.rpc is production already; the feature chapters exist); examples/reference/relay/README.md gains a "What the Relay exercises" table (feature or row, check, test, source path) read by r14-x-level-audit.'
  - 'Tier: example (publish = false). Secure by default: every new procedure declares its access decision (`r1_rpc_procedure_without_access_decision` keeps it at compile time); SSE and resume run their guards again (as `resumed_calls_run_their_guards_again` in id_effect_rpc); batch, stream window and resume buffers keep finite caps (R4); prod posture `relay_prod_posture_is_clean` with zero accepted risks.'
  - 'Coverage: example tier, no floor (ADR 0010); no library crate is edited.'
  - 'Gate: `ID_EFFECT_REQUIRE_DOCKER=1 ID_EFFECT_REQUIRE_DENO=1 cargo nextest run -p relay`; `cargo nextest run --cargo-profile release -p relay -E ''test(=relay_load_p99_within_budget)''`; `cargo clippy -p relay --all-targets --locked -- -D warnings`; `cargo run -q -p id_effect_tool --bin id-effect -- rpc check --contract examples/reference/relay/rpc.contract.json --against v0.13.0:examples/reference/relay/rpc.contract.json --generated examples/reference/relay/web/src/rpc`; CI job `reference-relay` green'
  - 'Owns: `examples/reference/relay/src/{api.rs,handlers.rs,node.rs,rooms.rs}` (new procedures, the SSE/batch/resume settings in the route builder), `examples/reference/relay/tests/{streams,transports,load}.rs` (new), `examples/reference/relay/tests/common/mod.rs` (append-only helpers), `examples/reference/relay/web/tests/relay.test.ts` (resume case), `examples/reference/relay/rpc.contract.json`, `examples/reference/relay/web/src/rpc/**` (generated), `examples/reference/relay/README.md`, `.github/workflows/ci.yml` (job `reference-relay`: a load step and its artifact upload)'
---

# r14-p-relay

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.2 Promote" (and the 0.12 follow-up F59,
0.14 part). The release spec's row is binding; this file exists so Maestro can track the leaf as
its own task.

Wave 4. Blocked by: r14-f-idempotency-pg (F53 may touch the Relay's `src/idempotency.rs`, not owned
here) and r14-f-h3spec (F62 edits the h3 adapter the Relay runs on). Runs beside r14-p-ledger,
r14-e-fuzz-security and r14-w-warden-mesh (only the `reference-relay` job of ci.yml). Blocks
r14-e-benches (the published p99) and r14-x-level-audit.

Defaults the owner may overturn: the load test is a nextest test run in the release profile in
`reference-relay`, not a criterion bench, so its p99 is not a `gates.toml` gate this release (no
v0.13 base to compare against); the budget is an absolute p99 on the shared runner, generous by
design, and the figure is published rather than gated tightly. WebSocket session resume is
proved with raw frames and the TypeScript client; adding `resume` to the Rust `WsClient` is not in
this leaf (a candidate 0.15 follow-up).
