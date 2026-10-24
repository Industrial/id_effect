# release-0-12 — execution overlay

Mission: created at kickoff (`maestro mission from-spec .maestro/specs/releases/0.12.0.md`) · spec
`.maestro/specs/releases/0.12.0.md` (approved under the owner's L15 goal; defaults D1–D14 open for
review, owner steps D15/D16) · branch `release/0.12.0` (off the `v0.11.0` tag after `release/0.11.0`
merged). Task ids are filled in by the coordinator when the tasks are created; until then the leaf slug
is the key.

## Preconditions (day 0)

- `v0.11.0` tagged; `docs/scorecard/results/v0.11.0.json` and `REPORT-0.11.0.md` committed (baseline).
- Owner has seen the six open questions in the release spec (defaults apply if unanswered).
- Bench runner allows `sudo tc` (D16) before r12-e12-5-benches and the h3-server spike.

## Wave 0 — head (dispatched at kickoff)

| Agent | Leaf (task id) | Files (owned) |
|---|---|---|
| A0 roadmap | `r12-t0-adr0014` (—) | docs/adrs/0014-one-call-surface.md, docs/scorecard/scorecard.json (targets), PLAN-OF-PLANS.md §5 T12 / §6 / §11, the release spec `scorecard:` block |
| A1 governance | `r12-t0-skeletons` (—) | docs/adrs/0010-crate-tiers.md (Amendment 8), root Cargo.toml members, labs/Cargo.toml, `git mv labs/id_effect_rpc crates/id_effect_rpc`, crates/id_effect_rpc_macros/ skeleton, examples/reference/relay/ skeleton, scripts/validate-crate-packaging.sh, .github/workflows/ci.yml (-p lines, labs job), docs/MIGRATION-0.12.md (headings) |

## Wave 1 (after A1 merges)

| Agent | Leaf (task id) | Files (owned) | Depends on / note |
|---|---|---|---|
| B1 rpc | `r12-e12-1-promote` (—) | crates/id_effect_rpc/** (all), r12-rpc-design.md, book ch07-12, ch29-00, ch33-00 | Skeletons. **Design note reviewed before wave 2's kinds starts.** Declares every rpc feature once. |
| B2 axum | `r12-e12-2-axum-ws-sse` (—) | crates/id_effect_axum/{Cargo.toml, src/realtime/**}, ws/sse tests + examples 090/091, book ch33-05 | Skeletons. Declares axum features ws/sse/tls/http3/webtransport once. |
| B3 quic | `r12-e12-3-quic-endpoint` (—) | crates/id_effect_platform/{Cargo.toml, src/tls/{mod,certs}.rs, src/quic/**}, crates/id_effect_node/{Cargo.toml, src/transport/{certs,quic,mod}.rs}, node tests transport_tls/shared_endpoint, book ch38-01 section | Skeletons (Amendment 8). Highest-risk leaf: review the D7 rule in its PR before merge. |

## Wave 2

| Leaf (task id) | Depends on |
|---|---|
| `r12-e12-1-kinds` (—) | promote + design-note review; critical path, start first |
| `r12-e12-3-tls-h2` (—) | axum-ws-sse (axum Cargo.toml), quic-endpoint (platform `tls`) |
| `r12-e12-3-h3-client` (—) | quic-endpoint |
| `r12-e12-3-certs-acme` (—) | quic-endpoint; Pebble container on its own port |

## Wave 3 (six leaves; at most five at once — start the first two immediately)

| Leaf (task id) | Depends on |
|---|---|
| `r12-e12-2-rpc-ws-sse` (—) | kinds, axum-ws-sse — **critical path** |
| `r12-e12-3-h3-server` (—) | tls-h2 — spike (D9) first; raise Open question 6 at once if HTTP/3 loses |
| `r12-e12-1-batching` (—) | kinds (also edits axum middleware/rate_limit.rs) |
| `r12-e12-1-macro` (—) | kinds, skeletons (macro crate) |
| `r12-e12-1-postcard` (—) | kinds |
| `r12-e12-2-subscriptions` (—) | kinds; own Postgres container (not the devenv DATABASE_URL) |

## Wave 4 (six leaves; at most five at once)

| Leaf (task id) | Depends on |
|---|---|
| `r12-e12-2-bidi-resume` (—) | rpc-ws-sse — **critical path** |
| `r12-e12-2-channels` (—) | subscriptions |
| `r12-e12-2-mesh-rpc` (—) | postcard |
| `r12-e12-4-openapi-asyncapi` (—) | macro |
| `r12-e12-5-benches` (—) | batching, rpc-ws-sse, h3-server, h3-client; idle machine; writes the unary delta gate only after measuring (D5) |
| `r12-e12-3-deploy-guide` (—) | h3-server, certs-acme (docs only; start last) |

## Wave 5

| Leaf (task id) | Depends on |
|---|---|
| `r12-e12-4-gen-ts` (—) | macro, batching, bidi-resume — **critical path** |
| `r12-e12-3-webtransport` (—) | h3-server, bidi-resume (first in the scope-cut order) |
| `r12-e12-5-fuzz-security` (—) | batching, postcard, macro, bidi-resume; TMPDIR under /data |

## Wave 6

| Leaf (task id) | Depends on |
|---|---|
| `r12-e12-4-rpc-check` (—) | gen-ts (cli.rs, rpc/mod.rs) |
| `r12-w6-kit-template` (—) | webtransport, gen-ts, channels, mesh-rpc, certs-acme, h3-server — **critical path** (also takes id_effect_tool Cargo.toml after gen-ts) |

## Wave 7

| Leaf (task id) | Depends on |
|---|---|
| `r12-w7-relay` (—) | kit-template, rpc-check — §6 KPI, never cut; owns the reference-relay job in ci.yml |
| `r12-w7-ledger` (—) | kit-template, rpc-check; owns reference-ledger job steps (rebase after relay) |

## Wave 8 and exit

| Leaf (task id) | Depends on |
|---|---|
| `r12-w8-level-audit` (—) | relay, ledger (read-only) |
| `r12-w8-kpis` (—) | relay, ledger, benches, fuzz-security; idle machine |
| `r12-t0-migration-guide` (—) | every leaf; then the blind re-score, REPORT-0.12.0, CHANGELOG, owner steps D15 (ADR 0014 accepted) |

Critical path: skeletons → promote → kinds → rpc-ws-sse → bidi-resume → gen-ts → kit-template → relay →
level audit. HTTP/3 (quic-endpoint → tls-h2 → h3-server) runs beside it and must be done before
webtransport (wave 5).

## Rules (as 0.10 / 0.11, plus the 0.11 lessons)

- One shared checkout: every leaf's Owns are disjoint within a wave (release spec "File ownership").
  Append-only shared files: CHANGELOG.md, docs/MIGRATION-0.12.md, book SUMMARY.md, Cargo.lock
  (regenerated on rebase), the `lib.rs` of id_effect_rpc / id_effect_axum / id_effect_platform after
  their wave-1 owner, and the module-declaration lines of the rpc `transport/`, `client/`, `server/`
  and axum `server/` `mod.rs` files.
- Agents never commit or stage. The main session reviews each leaf and commits it through a private
  index (`GIT_INDEX_FILE=.tmp/idx-<leaf> git add -- <paths>`, then `git commit -s -F <msg> -- <paths>`):
  pathspec-limited, DCO `Signed-off-by` required. Never run a bare `git commit` (0.10: a stray command
  staged other agents' work).
- Gates set `TMPDIR` under `/data` (the 2 GB `/tmp` tmpfs fills; fuzz and h3 tests are heavy). Live
  tests use `ID_EFFECT_REQUIRE_PG=1` / `ID_EFFECT_REQUIRE_DOCKER=1` / `ID_EFFECT_REQUIRE_BROWSER=1` and
  fail rather than skip; they never read the ambient `DATABASE_URL` (devenv overrides it) — each starts
  its own container on a distinct port.
- Bench gates are absolute where a ratio is ill-posed (the unary RPC delta, the secure-stack delta);
  ratios are kept only between two measurements of the same stack (batch vs sequential, h3 vs h2).
- Exact pins for h3 / h3-quinn / h3-webtransport; `cargo tree -d` must show one quinn, one rustls, one
  tungstenite; `reqwest_unstable` never appears.
- Never send the owner's email or other personal data to external services (crates.io lookups use a
  generic User-Agent).

## Follow-ups recorded during the release

| # | From | Follow-up | Owner (default) |
|---|---|---|---|
| F1 | skeletons | `id_effect_rpc_macros` never published: owner registers crates.io trusted publishing before the v0.12.0 tag | owner (D15/D16 list) |
| F2 | quic-endpoint | `templates/cluster/src/lib.rs.template` imports deprecated `kit::node::transport::certs`; move to `id_effect_platform::tls` | r12-w6-kit-template |
| F3 | quic-endpoint | node README `transport::certs` row is stale | r12-t0-migration-guide |
| F4 | quic-endpoint | `quic/endpoint.rs` 94.16% regions (crate total 96.4%); recheck at the workspace coverage gate | r12-w8-kpis |
| F5 | certs-acme | rcgen 0.13 + 0.14 (and untrusted, synstructure) duplicated under `acme` only; unify platform/node/sql on rcgen 0.14 | 0.13 (dependency hygiene) |
| F6 | certs-acme | ch29-09 not wired into `examples/book-snippets/src/part6.rs`; platform `lib.rs` feature line still says `acme` is "dependencies" | r12-e12-3-deploy-guide |
| F7 | certs-acme | deploy-guide content list (TLS passthrough for `acme-tls/1`, TCP 443, persistent 0700 cache, one ACME per domain set, alert on `AcmeStats::failed`, no DNS-01/wildcard, C13) | r12-e12-3-deploy-guide |
| F8 | rpc design review | anonymous idempotency scoped by client IP, key ≥ 16 chars (design note §10) | wherever the idempotency store lands (kinds/batching) |
| F9 | 0.11 exit | `effect:docs` moon output `target/doc` ignores a custom `CARGO_TARGET_DIR` | r12-w8-kpis (CI hygiene) |
| F10 | h3-client | HTTP/3 roots come from the system CA file; add `rustls-platform-verifier =0.7.1` to platform (Windows) | r12-e12-3-h3-server (owns platform http3 wiring next) |
| F11 | h3-client | reqwest `http2` is off in platform, so the TCP fallback is HTTP/1.1 (test `udp_blackhole_falls_back_to_h2_within_budget` exercises h1.1); turn `http2` on and keep the test name honest | r12-e12-3-h3-server |
| F12 | h3-client | h3 transport errors surface as `HttpError::InvalidRequest("http3: …")`; add a `Transport` variant (breaking, MIGRATION-0.12) | r12-e12-5-fuzz-security or exit |
| F13 | h3-client | h3-quinn `stop_sending` panics during an in-flight read; drop the stream instead (server too) | r12-e12-3-h3-server |
| F14 | tls-h2 | HTTP/2 idle-connection loosening has no `dangerous_*` opt-out because hyper exposes no h2 idle timeout; idle live h2 is bounded by the connection cap and dead-peer pings only. Revisit when hyper adds it; document in THREAT-MODEL | r12-e12-5-fuzz-security (threat model) |
| F15 | tls-h2 | RFC 8441 extended CONNECT off (browsers use h1.1 for WebSockets) | 0.13 |
| F16 | kinds | `x-rpc: 1` now required on every POST (stricter than spec C5) and POST without Content-Type is 415 — record in MIGRATION-0.12 | r12-t0-migration-guide |
| F17 | kinds | ch33 chapters not compiled by `book_snippets` | r12-t0-migration-guide |
| F18 | postcard | postcard over plain HTTP answers 400 (server/http.rs decodes untyped); wire `decode_call` / `entry.codecs().decode_input` / `encode_exit` + `encode_result`, and the client | r12-e12-1-batching (reassigned by coordinator) |
| F19 | postcard | fuzz targets: `codec::postcard::{decode_frame, decode_call, decode_result, decode_exit, split_record, decode_limited}`; seed a recursive type and a long empty-element sequence | r12-e12-5-fuzz-security |
| F20 | postcard | macro must emit `.with_serde()` for serde types so generated services speak postcard | r12-e12-1-macro (sent) |
| F22 | h3-server (D9) | Spike via userspace loss proxies (no `sudo tc`): 1 % loss + 5 ms, 64×16 KiB, p99 — h2 609 ms, h3 Cubic 1,054 ms (1.73, fails), h3 BBR 31 ms (0.05). **Coordinator decision:** the one D9 tuning task is BBR — platform `QuicLimits` gains a congestion-control setting, BBR default on the `h3` route only, mesh stays Cubic (quinn marks BBR experimental). The pair gate stays, measured by r12-e12-5-benches on the real netem runner; if BBR loses there, the gate becomes report-only (D9) | coordinator follow-up agent (platform) + r12-e12-5-benches |
| F23 | h3-server | UDP receive-buffer drops (1,100–2,000/run) on quinn sockets; raise socket buffers on the endpoint; deploy guide: `net.core.rmem_max` | coordinator follow-up agent + deploy-guide |
| F24 | h3-server | `HostTls::Provided` serves TCP only (no shareable identity); carry an `Identity` so it serves h3 too | coordinator follow-up agent |
| F25 | h3-server | no early-data setting on the platform endpoint; `dangerous_allow_zero_rtt_for_queries` is a recorded no-op until one exists | 0.13 |
| F26 | h3-server | `http3-conformance` CI job (h3spec) unverified locally (sandbox blocks binaries); add to `ci-pass` once green in CI | r12-w8-kpis |
| F27 | batching | macro-generated `HttpTransport` builds its own request, so generated Rust clients do not get same-tick batching; route it through `RpcClient::call` | r12-e12-4-rpc-check (touches client generation) or exit |
| F28 | batching | fuzz target `wire::decode_batch(bytes, max_calls, max_bytes)`; a guard refusal answers the whole batch with `x-rpc-batch-index` (document in threat model) | r12-e12-5-fuzz-security |
| F29 | channels | call handlers need `CurrentPrincipal` and a per-socket `ConnectionId` in `DynEnv` (else anonymous, and one connection per call) | r12-e12-2-rpc-ws-sse (sent), r12-e12-2-bidi-resume |
| F30 | channels | `channels::attach_session` runs a mesh over a session because id_effect_node has no session pump; move it into node | 0.13 |
| F31 | channels | `tests/ui.rs` skipped under feature `channels` because `error_type_not_a_union.stderr` lists every `RpcErrorUnion` impl; make the UI case robust (custom `on_unimplemented` without impl list, or a narrower bound) | r12-e12-5-fuzz-security (owns compile-fail) |
| F32 | channels | kit `cluster` feature turns on `id_effect_rpc/channels`; nodes call `register_hub` before `Mesh::new`, `start_on_mesh`, `spawn_heartbeat`; QUIC dial and accept run concurrently with the session handshake | r12-w6-kit-template, r12-w7-relay |
| F33 | mesh-rpc | `channels::attach_session` and `MeshFabric` in-memory links call `mesh.handle_envelope_from` directly, so mesh RPC calls over them are dropped as unverified (fails closed); route through `MeshRpc::ingress` when `mesh` is on | coordinator fix |
| F34 | mesh-rpc | `MeshTransport` (generated clients) carries no trace context; streams detect a dead node only via `mesh.peer_down` (no heartbeat); `client/mesh.rs` 94.48 % regions (new code 96.4 %) | 0.13 / r12-w8-kpis coverage gate |
| F35 | mesh-rpc | incident: a stale-tree `update-ref` briefly pointed the branch at `ec0de21c` (reverting batching + channels); restored by CAS within minutes, history verified intact. Protocol fix: build the tree with `read-tree` on the CAS `<old>` itself, never on an earlier HEAD | protocol (agent-protocol-012.md) |
| F36 | h3-platform | axum host / `serve_h3` always register `Route::h3()` (BBR); add a congestion-control switch in `Http3Options` so operators can fall back to Cubic without code | r12-e12-5-benches (if BBR fails on netem) or 0.13 |
| F37 | h3-platform | axum HTTP/3 integration tests flake under the full parallel suite (5 s handshake wait; reproduced on 8e80ce65 without the change); give them a nextest test-group with max-threads or a longer handshake wait | r12-w8-kpis (CI stability) |
| F38 | rpc-ws-sse | `Rejected` exit tag not in `RpcFailure::RESERVED_TAGS` (a user variant could claim it) | r12-e12-2-bidi-resume (assigned, owns wire.rs hunk) |
| F39 | rpc-ws-sse | realtime 100 msg/s per socket counts credit frames too, throttling fast client streams; consider not counting `credit`/`ping` or a per-RPC default | r12-e12-5-benches (soak) / 0.13 |
| F40 | rpc-ws-sse | Rust `WsClient` is JSON-only over plain `ws://` (`wss://` via `WsClient::over`); add a TLS connector + postcard | r12-w6-kit-template or 0.13 |
| F41 | fuzz-security | `security_surface.rs` `PENDING`: `r9_webtransport_connect_from_foreign_origin_refused` (webtransport, sent) and `posture_prod_refuses_dev_certificate` (kit-template); list must be empty at exit | r12-e12-3-webtransport, r12-w6-kit-template, exit check |
| F42 | fuzz-security | `r10_zero_rtt_on_mutation` only proves the sole opt-out admits safe methods (no per-procedure 0-RTT API); `security_compile_fail` depends on rpc/axum directly until kit gains `rpc`/`http3` features — move the cases to kit | r12-w6-kit-template |
| F43 | webtransport | host entry points (`ServerHandle`, `serve_router`) keep WebTransport off; only `serve_h3` + `Http3Options::webtransport()` on a `webtransport::quic_limits` endpoint serves it — wire it into the host | r12-w6-kit-template (offered) or 0.13 |
| F44 | webtransport | axum `tests/http3.rs` timing tests fail under load ≈ 17 (10/11 alone); same as F37 | r12-w8-kpis (CI stability, with F37) |
| F45 | webtransport | no Deno WebTransport test of the TS client (browser test covers the Rust/JS page path) | 0.13 |
| F46 | rpc-check | no override to retire an old version on purpose (dropping a version always fails); add `--allow-retired Service:N` | 0.13 |
| F47 | rpc-check | classification deviates from spec wording: a narrowed output is compatible (old clients still read it); a removed input field is compatible unless unknown fields are refused — record in ch33-12 (done) and ADR 0014 at exit | r12-t0-migration-guide |
| F27 | (update) | reassigned to coordinator follow-up leaf r12-f-client-batching (tsk-mvfohzqk-xg47t7) | r12-f-client-batching |
| F48 | benches | axum host listener / `axum::serve` lack `TCP_NODELAY`: WS p99 41-42 ms (Nagle + delayed ACK) vs ~1 ms with it | r12-f-unary-perf |
| F49 | benches (D5) | unary delta +3,399 ns > 3,000 ns; benches task tsk-mvfixl4d-wu25gw blocked until r12-f-unary-perf enables the gate; coordinator decision: one perf pass, budget stands; owner sees the floor at exit if unreachable | r12-f-unary-perf → unblock + ship benches |
| F50 | benches | soak 10 min RSS 1.040 vs 1.05 limit — thin after resume (16 → 35 KiB/stream, warm-up to ~min 5, bounded); 60 s soak mark moved 10 s → 30 s. Decide at r12-w8-kpis: mark at minute 5 vs a smaller default replay window | r12-w8-kpis |
| F51 | ledger | per-request remount workaround in `examples/reference/ledger/src/rpc.rs::mount` is obsolete after 5db2e2e4 (HTTP calls see RequestCaps) — remove | r12-t0-migration-guide (exit sweep) |
| F52 | ledger | no public helper to lift an `Effect<_, _, ()>` into a procedure's `DynEnv` effect (pattern: `Effect::from_ctx(|ctx| async move { ctx.bind(e).await })`) | 0.13 |
| F53 | ledger/relay | Postgres idempotency store exists twice (Ledger `PgIdempotencyStore`, Relay `relay_idempotency`); promote into id_effect_rpc / sql | 0.14 (planned stores) |
| F54 | relay | coverage of relay 93.7 % regions (96 % target; misses = DB-fault paths, binary exits); cargo-llvm-cov needs `--disable-default-ignore-filename-regex` for examples/ | r12-w8-kpis (decide; examples are outside the workspace coverage gate) |
| F55 | relay | kit lacks an `id_effect_process` re-export (Relay + cluster template depend on it directly) | 0.13 |
| F56 | relay | generated Rust `HttpTransport` cannot attach headers (bearer tokens); tests wrap the HTTP client | 0.13 (with F27 follow-up) |
| F57 | relay | a live client stream consumed with `run_for_each` stalls until a full default-size chunk arrives | 0.13 (id_effect Stream chunking; see subscriptions note on `unfold_effect`) |
| F58 | relay | **bug**: TS `RpcStream.cursor` is the last item received incl. buffered-unread, so resuming from it can skip unread messages | r12-f-ts-cursor (coordinator fix leaf) |
| F59 | relay | WebTransport not in the host: Relay serves h3 on its own endpoint with host h3 off (= F43); `reference-relay` CI job (iptables UDP drop, runner Chrome) unproven locally; server streams, client streams, SSE, postcard, batching and WS session resume not exercised by Relay | r12-w8-level-audit (record), 0.13 |
| F27 | (deferred) | r12-f-client-batching blocked → 0.13: works, no breaking change, but ~135 lines with a per-caller-env batch resolver table because `id_effect` has no public helper to run an env-free effect under any env. 0.13: add that helper first (also F52), then apply `.maestro/specs/releases/0.13/f27-client-batching.patch` (applies on a344bd9e), shrinking it | 0.13 |
| F60 | kpis | F50 decided: soak RSS mark moves to minute 5 (0.997; minute-1 1.040 still reported) — the climb is bounded replay-buffer warm-up; shrinking the replay window would weaken resume. Change `crates/id_effect_rpc/examples/streams_soak.rs` + bench.yml/gates | r12-t0-migration-guide (exit) |
| F61 | kpis | security run builds the workspace `--all-features` (30+ min loaded); `/tmp` too small for `DX_KPIS_DIR` — use `.tmp/`; stale-artifact `QuicLimits::with_datagrams` not found in an old target dir → `cargo clean -p id_effect_platform` | note |
| F62 | CI (PR #112) | first `http3-conformance` run: h3spec v0.1.14 77 examples, 19 failures. **Ours (server/h3.rs adapter):** connection-specific headers not rejected as malformed (connection, keep-alive, proxy-connection, te, transfer-encoding, upgrade — HTTP/3 4.2), duplicated / misplaced pseudo-headers (4.1.1, 4.1.3), frame longer than SETTINGS_MAX_FIELD_SECTION_SIZE buffered (7.1). **h3 0.0.8:** QPACK encoder/decoder stream errors (QPACK 4.1.3, 4.2, 4.4.3), SETTINGS mid-parameter / repeated id (7.1, 7.2.4.1), CANCEL_PUSH on a request stream (7.2.5), client push stream (6.2.2). **quinn/rustls:** missing_extension alert without quic_transport_parameters (TLS 8.2). Job is not in `ci-pass` (F26). Low risk today (the server terminates h3; those headers never frame bodies) | 0.13 leaf: adapter validation + upstream h3 issues/PRs; add the job to ci-pass when green |
| F63 | CI (PR #112) | flaky: `id_effect_sql::conformance mysql::dropped_streams_release_their_connections` failed once in Container integration (2627692d: `SELECT 1 AS n: sql client not connected` after 88 s, i.e. pool acquire timed out); passed on aa84c378 and #111 with no sql source change between 0.11 and 0.12. Make it deterministic (wait for the pool's idle count instead of a fixed timeout; check the MySQL container's max_connections) | 0.13 |
| F64 | main CI (#113) | Windows: the first sysinfo sample blocks the calling thread for ~350 ms (CI: 370.9 ms stable, 343.8 ms beta) in `ComputeFabric::tick` / `SysinfoTelemetry::snapshot`. #113 relaxed the two non-blocking tests to a 1 s bound on Windows only, keeping 50 ms elsewhere (`crates/id_effect/src/compute/{fabric,telemetry}.rs`). Real fix: take the first sample off the calling thread (spawn it, return a neutral snapshot until it lands), then restore the 50 ms bound everywhere. | 0.13 |
| F65 | main CI (#113) | MySQL conformance `dropped_streams_release_their_connections`: in CI a dropped MySQL stream returns its connection to the pool only after the server sends the rest of the result. The final `SELECT 1` timed out after ~80 s (Coverage, Container integration) and still after the 120 s retry (beta / ubuntu: 142 s), against ~6 s locally. #113 cut the streams to 10,000 rows and kept a bounded retry on NotConnected (`crates/id_effect_sql/tests/conformance/mod.rs`). Investigate why the drain is 20x slower in CI: the runner's MySQL container, sqlx-mysql's drain-on-return, or our stream driver holding the connection until the producer task ends. Consider closing the connection (or KILL QUERY) instead of draining when a large result is dropped. | 0.13 |
| F21 | subscriptions | no cursor = replay from `starting_after` (default whole log), not "from now" — `EventLog` has no head read; document, or add a head read in id_effect_events | 0.13 / document in ch33-08 |
