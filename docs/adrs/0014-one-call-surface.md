# ADR 0014 — One call surface

## Status

Proposed (2026-10-19). Release 0.12.0, track T12, leaf `r12-t0-adr0014`. The owner moves it to
Accepted ([0.12.0 D15](../../.maestro/specs/releases/0.12.0.md)); the 0.12.0 exit gate needs
"Accepted" and every rule's enforcement point green at the tag. Until then every 0.12 leaf cites
this ADR as Proposed. The decisions come from the release spec's "Owner decisions" table (D1–D14)
and its "Security — ADR 0013 applied to the call surface" table, which follow the One Call Surface
design document (2026-10-18) and owner decision L15 (PLAN-OF-PLANS §3.1). Where those disagree,
PLAN-OF-PLANS wins.

Owner decisions already taken (not re-asked): RPC replaces GraphQL as the first-party call surface
(L15); `id_effect_rpc` leaves `labs/`; realtime channels are a module of `id_effect_rpc`, not an
`id_effect_channels` crate; HTTP/3 is a standard transport, on by default in the `service`
template. The six open questions of the release spec are recorded below with the defaults this ADR
takes; the owner can overturn any of them before accepting it.

## Context

At 0.11.0 id_effect has three ways to call a service and none of them is the platform's:

- **REST + OpenAPI** (`id_effect_axum` typed routes and its OpenAPI output). Good for public APIs,
  but the client is hand-written and nothing types a stream, a batch or a subscription.
- **Labs RPC** (`crates/id_effect_rpc`, 2,141 lines, `publish = false`): tagged JSON over
  `POST /rpc`, field-name lists, NDJSON server streams, OpenAPI 3.0 output. No macro, no client
  derived from a contract, one transport, no batching, no two-way streams, no binary encoding, no
  access decision per procedure, no cancellation contract. Scorecard `web.rpc` 39 (level labs).
- **GraphQL** was planned as E9.2 and is deleted by L15; `web.graphql` is a non-goal row since
  method 2.2.

Realtime is absent (`web.realtime` 1): `channel_bridge.rs` is a request/response bridge, axum runs
without its `ws` feature, and the mesh has process groups but no topics or presence. The mesh
already speaks QUIC with mutual TLS by default (0.11, `dist.tls` 89), on its own endpoint, and the
HTTP host speaks plain TCP with HTTP/1.1 only.

L15 asks for one call surface before the 1.0 freeze, because its contract (procedure kinds, error
unions, cursors, the wire format) is API that a 2.0 would otherwise have to break. ADR 0013 applies
to every new surface: deny by default, finite limits, every weakening a recorded `dangerous_*`
opt-out.

## Decision

### 1. RPC is the first-party call surface; REST + OpenAPI stays the public API style

- `#[rpc::service]` on a trait is how id_effect services call each other and how first-party
  clients (Rust, the generated TypeScript client, browsers, other nodes) call a service. One
  contract serves HTTP, WebSocket, SSE, HTTP/3, WebTransport and the mesh.
- REST + OpenAPI stays the recommended style for **public, third-party** APIs. The RPC contract
  exports OpenAPI 3.1 and AsyncAPI as its language-neutral description (`web.rpc#7`), so a public
  consumer never needs the RPC client.
- **GraphQL is a non-goal** (L15): no gateway, schema export or federation. Procedures are
  purpose-built; an optional `include` parameter is the only shaping tool. Client-chosen fields
  and nested graph queries are not offered.
- gRPC/protobuf wire compatibility is not planned before 1.0.

### 2. Procedures, errors and cancellation

Six procedure kinds, each a method returning `Effect<A, E, R>`:

| Kind | HTTP mapping | Notes |
|---|---|---|
| `query` | GET `?input=` with ETag/304, or POST | retry-safe; the only kind 0-RTT may ever serve |
| `mutation` | POST with `Idempotency-Key` | replay returns the stored result (D14) |
| `server_stream` | NDJSON response | pull-based, so a slow client slows its stream |
| `client_stream` | chunked NDJSON request | item and byte caps |
| `bidi` (two-way) | WebSocket / WebTransport | credit backpressure, resume cursors |
| `subscription` | SSE / WebSocket | resumable from a cursor (events: the global position) |

Typed errors are a tagged union `{"_tag": …}` and are part of the contract; a defect becomes
`InternalError { id }` with only the correlation id. A client disconnect interrupts the handler
fiber and runs its finalizers; the server caps each call's deadline. A wire batch is a JSON array
of calls with ids echoed, results per call, one `RequestCache` per batch so duplicate loads merge.

### 3. Crate placement (D3)

- `crates/id_effect_rpc` (moved from `labs/`) and `crates/id_effect_rpc_macros` (proc-macro crates
  cannot export runtime items). Both enter tier `integration` (ADR 0010 Amendment 8, leaf
  `r12-t0-skeletons`); `id_effect_rpc` moves to `platform` when the Relay reference app depends on
  it (ADR 0010 check P), which the 0.12.0 exit gate requires.
- Realtime channels are `id_effect_rpc::channels` (topics on mesh process groups, presence,
  `JoinPolicy`). There is no `id_effect_channels` crate (owner, L15).
- WebSocket and SSE primitives live in `id_effect_axum` (features `ws`, `sse`); RPC transports
  over them live in `id_effect_rpc`.

### 4. HTTP/3 is a standard transport (D4)

- HTTP/3 is on by default in the `service` template and the zero-to-production tutorial (the
  "starter"). One config flag turns it off: `server.http3.enabled` (default `true` in the
  template). No `--no-http3` CLI flag is added.
- **TCP is never removed.** HTTP/3 is advertised by `Alt-Svc: h3=":<port>"` on TCP responses, so a
  client upgrades only after a TCP response; UDP blocked means the client stays on HTTP/2. The
  platform client falls back after 300 ms and caches the failure for 5 minutes.
- **One shared port by default:** TCP and UDP listen on the same port number, so one firewall rule
  (UDP and TCP 443) is the whole change for direct-to-app HTTP/3.
- The h3 / h3-quinn adapter is one thin module per crate, with exact pins (`h3 =0.0.8`,
  `h3-quinn =0.0.10`, `h3-webtransport =0.1.2`), so a later swap to hyper's HTTP/3 is a separate,
  local change. reqwest's HTTP/3 (`--cfg reqwest_unstable`) is never used; the platform has its own
  client on `h3`, and a grep gate fails the build if `reqwest_unstable` appears.
- Load balancers that terminate HTTP/3 are documented (deploy guide), not built. No first-party
  CDN or edge terminator.

### 5. The shared QUIC endpoint lives in `id_effect_platform` (D6)

- `id_effect_platform` gains features `tls` (certificates, reload, rustls configs) and `quic`
  (`QuicEndpoint`, `AlpnRouter`). One QUIC endpoint on one UDP port routes by ALPN: `h3` to the
  HTTP/3 adapter, `idm/1` to the mesh.
- No dependency cycle: `id_effect_node` gains `id_effect_platform` (optional, under its
  `quic`/`tls-tcp` features); `id_effect_axum` uses it under `http3`. Today neither node nor
  platform depends on the other.
- The node certificate code (`transport/certs.rs`: `NodeCertificate`, `CertReloader`, `Identity`)
  moves to `id_effect_platform::tls`, with a `#[deprecated]` re-export in node for one minor (L9).
- Accepted cost: node's TLS features pull reqwest through `id_effect_platform` (compile time for
  mesh-only users and the `cluster`/`worker` templates; the K6 delta is reported at exit). The
  alternative, a small `id_effect_quic` crate, is rejected below.
- The mesh requires ALPN `idm/1`. A 0.11 node cannot join a 0.12 QUIC mesh (mixed-version mesh
  interop stays a non-goal); mixed-version RPC clients are supported through contract versions.

### 6. Client authentication on the shared endpoint (D7)

rustls fixes the client-certificate verifier per `ServerConfig`, before ALPN is known, so "mutual
TLS for `idm/1`, none for `h3`" cannot be a handshake rule on one endpoint. The rule is:

- When both ALPNs are registered on one endpoint, the server **requests** a client certificate as
  optional, with **no CA hints**. A presented certificate is still verified against the cluster CA
  during the handshake.
- The `idm/1` handler **refuses any connection without a verified client chain before reading a
  frame**. `dist.tls#4` (mutual authentication of mesh peers) therefore holds on the shared shape.
- `h3` ignores client certificates.
- `quic.mesh_port` (config) splits the mesh onto its own endpoint with handshake-level mutual TLS.
  The 0.11 mTLS tests pass on both shapes.
- **The shared single-port shape is the default** when a node serves both HTTP/3 and the mesh; the
  split shape is one config key away. Browsers with installed client certificates may show a
  certificate picker on the shared shape: documented. The `service` template has no mesh, so it
  never uses the shared shape.

### 7. TLS and HTTP/2 on TCP in the host (D10)

`id_effect_axum` gains feature `tls`: TLS 1.3/1.2 on TCP with ALPN `h2`/`http/1.1`, using the
platform `tls` identity and its reload (new connections only). Browsers honour `Alt-Svc` only on
HTTPS, so direct-to-app HTTP/3 needs this, and the client's HTTP/2 fallback is tested against our
own server. The 0.11 connection limits apply to TLS and HTTP/2 (max concurrent streams, header
timeout). Deployments behind a TLS-terminating load balancer need none of it.

### 8. Certificates and ACME (D8)

Certificate reload drives both the QUIC endpoint and the TCP listener (new connections only;
`dist.tls#7` kept). ACME is TLS-ALPN-01 only (`acme-tls/1`, answered by the rustls certificate
resolver on the TCP TLS listener, one port), through `instant-acme`, behind platform feature
`acme`, tested against a pinned Pebble container. Renewal at two thirds of the lifetime; a failure
keeps the current certificate. DNS-01 is a non-goal. `id-effect dev` issues a 13-day ECDSA
development certificate, which the prod profile refuses.

### 9. The contract artifact (D11)

The macro generates `Service::contract() -> Contract`. A golden test
(`id_effect_rpc::contract::assert_snapshot`) keeps `rpc.contract.json` in the crate root current
(`UPDATE_GOLDEN=1` rewrites it, as `id_effect::testing::golden`). `id-effect rpc gen ts` and
`id-effect rpc check` read that JSON; the CLI has no compile step. `rpc check` fails on a breaking
change to a published contract version; versions are served side by side.

### 10. Encodings (D13)

JSON by default. postcard is negotiated per connection (`application/vnd.id-effect.rpc+postcard`,
WebSocket subprotocol `idrpc.postcard.v1`; JSON is `idrpc.json.v1`) and is always used node to
node. The postcard path decodes with serde and then runs the Schema's validators, so refinements
still apply. An unknown codec is a 415 Problem. CBOR and MessagePack are not offered.

### 11. Mutation idempotency (D14)

An `IdempotencyStore` trait with a bounded in-memory store (TTL 24 h, 100,000 keys) in 0.12.
Postgres and Redis stores are 0.14 sweep items.

### 12. TypeScript client and browser tests (D12)

The generator emits plain TypeScript with no runtime dependency. **Deno runs the TypeScript client
tests:** `deno check` and `deno test` on one pinned Deno (≥ 2.4, which also bundles for the browser
tests). Chromium is driven from Rust tests over CDP. No `package.json`, `node_modules` or lockfile
enters the tree (L12 spirit).

### 13. Performance budgets (D5, D9)

- **Unary overhead is an absolute budget (D5):** a `delta` gate
  `rpc/unary_json − rpc/axum_json_handler ≤ 3 µs` per call, same run and same runner. The baseline
  is an axum handler doing the same serde JSON decode and encode of the same payload. The ratio is
  printed for information only. The earlier PLAN-OF-PLANS gate "unary ≤ 1.10× raw axum" is
  replaced: since 0.11 runs request fibers inline on the caller's task (`bf7be935`), a bare axum
  request costs 0.90 µs, so 1.10× would leave about 90 ns for envelope decode, Schema decode and
  encode, the access check, error mapping and the span. No implementation of those fits, and the
  ratio would mostly measure how fast the denominator got: the same trap as the 0.11 secure-stack
  ratio (owner decision 2026-10-19). `r12-e12-5-benches` measures the number before it writes the
  gate; the owner confirms 3 µs as the ceiling.
- Batch: `rpc/batch_50 / rpc/sequential_50 ≤ 0.20` (pair gate, ≥ 5× faster).
- Streams: 10,000 concurrent server streams on one node, RSS growth ≤ 5 % between minute 1 and
  minute 10, every stream heartbeats (soak).
- **HTTP/3 under loss (D9):** pair gate `h3_p99 / h2_p99 ≤ 1.00` (noise 0.05) on 64 multiplexed
  16 KiB streams over one connection, loopback with `tc netem loss 1% delay 5ms`. If the first
  measurement (the `r12-e12-3-h3-server` spike) fails it, **one tuning task** is added (congestion
  control, stream and flow-control windows, pacing); if HTTP/3 still loses after it, the gate
  becomes **report-only** for 0.12 with a written carry-over, and the measured numbers are
  published either way.
- The secure-stack budget (≤ 5 µs over the bare request) is unchanged.

### 14. Security rules for the call surface (ADR 0013 applied)

Every new surface follows ADR 0013. Each rule below holds while its enforcement point is green;
the leaf named owns the enforcement point.

| # | Rule | ADR 0013 | Enforcement point (mechanical) | Leaf |
|---|---|---|---|---|
| C1 | Every procedure declares its access: `#[rpc::service]` refuses a procedure without `#[public]` or `#[guard(G)]`; the low-level `RpcGroup` uses the same typestate as H8 routes; an unknown tag is a 404 Problem. | R1 | compile-fail `r1_rpc_procedure_without_access_decision`; runtime `rpc_unregistered_tag_is_404_problem` | r12-e12-1-macro, r12-e12-1-promote |
| C2 | A batch is bounded (≤ 50 calls, ≤ 1 MiB body, config) and is charged N calls by the rate limiter. | R4 | `r4_batch_over_limit_rejected_413`, `r4_batch_charges_n_rate_limit_tokens` | r12-e12-1-batching |
| C3 | Streams and connections are bounded: streams per connection, frame size (64 KiB), message rate, idle timeout and heartbeats (ping 20 s, two missed close); QUIC stream credits and idle timeout are finite. | R4 | `r4_ws_streams_per_connection_capped`, `r4_ws_frame_over_max_closes_1009`, `r4_quic_limits_finite_by_default` | r12-e12-2-axum-ws-sse, r12-e12-2-rpc-ws-sse, r12-e12-3-quic-endpoint |
| C4 | `Origin` is checked on every WebSocket upgrade and WebTransport CONNECT against an allowlist; an empty list refuses every browser origin; `dangerous_any_origin` is recorded. | R9, R10 | `r9_ws_upgrade_from_foreign_origin_refused`, `r9_webtransport_connect_from_foreign_origin_refused`, `dangerous_any_origin_is_recorded` | r12-e12-2-axum-ws-sse, r12-e12-3-webtransport |
| C5 | Cookie-authenticated mutations need `Content-Type: application/json` (or postcard) and the `x-rpc` header; the 0.11 CSRF layer applies. | R9 | `r9_rpc_mutation_without_custom_header_refused`, `r9_rpc_form_content_type_refused` | r12-e12-1-kinds |
| C6 | QUIC address validation (Retry) is on, with quinn's anti-amplification limit. | R4, R8 | `r4_quic_retry_on_by_default`; posture probe "quic retry on" | r12-e12-3-quic-endpoint, r12-w6-kit-template |
| C7 | 0-RTT is off. `dangerous_allow_zero_rtt_for_queries()` allows it for `query` procedures only, never for mutations, and is recorded. | R1, R10 | compile-fail `r10_zero_rtt_on_mutation`; `zero_rtt_off_by_default` | r12-e12-3-h3-server, r12-e12-5-fuzz-security |
| C8 | Resume session ids are 128-bit random and bound to the authenticated principal. | R1 | `resume_under_another_principal_refused` | r12-e12-2-bidi-resume |
| C9 | Hostile payloads: Schema decode at the boundary, size caps before buffering, a fuzz target on every new decoder (wire request, batch, postcard frame, duplex frame, resume cursor, SSE `Last-Event-ID`). | R4 | ≥ 5 new cargo-fuzz targets in ClusterFuzzLite, no open crash | r12-e12-5-fuzz-security (decoders in their leaves) |
| C10 | Typed errors are the contract; a defect becomes `InternalError { id }` with the correlation id only. | R3 | `defect_becomes_internal_error_without_message` | r12-e12-1-kinds |
| C11 | Joining a channel goes through a `JoinPolicy`; the default denies. | R1 | `channel_join_denied_by_default_policy` | r12-e12-2-channels |
| C12 | `/rpc/openapi.json`, `/rpc/asyncapi.json` and `/rpc/_contract` are closed in prod unless guarded. | R7 | `r7_rpc_docs_closed_in_prod` | r12-e12-4-openapi-asyncapi |
| C13 | The `id-effect dev` certificate is refused by the prod profile. | R10 | `posture_prod_refuses_dev_certificate` | r12-w6-kit-template |
| C14 | On the shared QUIC endpoint, `idm/1` refuses a connection without a client chain verified against the cluster CA before reading a frame (D7). | R8 | `idm_without_client_cert_refused_on_shared_endpoint`, `idm_cert_from_other_ca_refused_on_shared_endpoint`; the 0.11 `mtls_rejects_peer_without_client_cert` on both shapes | r12-e12-3-quic-endpoint |
| C15 | The HTTP/3 client keeps the 0.11 outbound guards (SSRF policy, timeouts, body caps) and never uses `reqwest_unstable`. | R6 | `h3_path_enforces_ssrf_policy`, `h3_body_cap_enforced`; grep gate on `reqwest_unstable` | r12-e12-3-h3-client |

The development profile allows `http://localhost:*` origins by configuration (not by a dangerous
opt-out), and the TypeScript client always sends `x-rpc`, so C4 and C5 do not break local work.
Every `dangerous_*` API introduced here calls `posture::record_dangerous` and is listed by
`scripts/check-dangerous.sh`.

### 15. Scorecard effect

`web.rpc` targets 100 at 0.12.0 (8/8 checks at level production: the Relay and the Ledger exercise
it in CI; D2). `web.realtime` targets 88. `dist.tls` stays at or above 88 with the mesh on the
shared endpoint. The six 0.12.0 targets that predate L15 (`data.orm`, `web.ui`, `web.admin`,
`ai.llm`, `eco.i18n`, `eco.tenancy`) belong to 0.13 "Breadth" and are retargeted to 0.13.0 with
their values unchanged (D1). From the measured 0.11.0 baseline, `scripts/scorecard.sh --targets
0.12.0` gives E 80.9 (#1), RT 84.8 (#1), 7 red rows; `--targets 0.13.0` gives E 84.3 (#1), RT
86.6, 3 red rows.

## Consequences

Positive:

- One contract, many transports: a procedure written once is callable over HTTP, WebSocket, SSE,
  HTTP/3, WebTransport and the mesh, from Rust, TypeScript and other nodes, with the same typed
  errors and cancellation.
- The contract is a checked-in artifact, so breaking changes fail CI (`rpc check`) instead of
  surfacing in a client.
- The call surface inherits ADR 0013 from the start: every procedure declares its access, every
  limit is finite, every weakening is named and recorded.
- HTTP/3 on by default costs a service nothing when UDP is blocked (TCP stays, the client falls
  back) and one flag turns it off.

Negative:

- The h3 stack is pre-1.0 (`h3` 0.0.8): breaking changes each release, tied to quinn 0.11. Exact
  pins and thin adapters contain it, but every bump is work.
- Mutual TLS on the shared endpoint is enforced after the handshake (D7), not by it. It is checked
  before any frame and tested, but it is a weaker shape than handshake-level mTLS, and some browsers
  may show a certificate picker on it. `quic.mesh_port` restores handshake-level mTLS.
- Node's TLS features pull `id_effect_platform` and with it reqwest (compile time for mesh-only
  users); HTTP/3 on in the template adds quinn, h3 and rustls to K1/K2/K6.
- Semver breaks: the node certificate module moves (deprecated re-export for one minor), labs
  `RpcGroup::register` gives way to `#[rpc::service]`, OpenAPI output moves 3.0 → 3.1, and 0.11
  nodes cannot join a 0.12 QUIC mesh. Each is listed in `docs/MIGRATION-0.12.md`.
- No GraphQL means clients that want client-chosen fields get purpose-built procedures with
  `include` instead.
- 0.12 plans E 80.9 rather than the 84.2 the stored targets implied, because the six breadth rows
  move to 0.13.

## Alternatives considered

- **GraphQL as the call surface (async-graphql, E9.2).** Rejected by the owner (L15): it adds a
  second schema language, needs its own batching and authorization layer, and its strength
  (client-chosen fields) is not what id_effect's services need. `web.graphql` is a non-goal row.
- **gRPC / tonic.** Rejected: protobuf would be a second IDL beside Schema, browsers need a proxy
  (gRPC-Web), and streams would not share the effect-native cancellation and resume model. OpenAPI
  3.1 and AsyncAPI are the language-neutral exports.
- **A separate `id_effect_channels` crate.** Rejected by the owner (L15, D3): channels share the
  contract, transports and resume model of RPC; a crate boundary would duplicate them.
- **A new `id_effect_quic` crate for the endpoint (release spec Open question 3).** Rejected for
  0.12: one more crate for the 0.18 crate review, for a compile-time saving that only mesh-only
  users see. The move is mechanical (one `tls` + `quic` module tree) if the K6 delta at exit says
  otherwise.
- **Separate ports by default for HTTP/3 and the mesh (Open question 5).** Rejected as the
  default: one port is the point of the shared endpoint, and C14 keeps mutual authentication of
  mesh peers. Kept as a one-key option (`quic.mesh_port`).
- **A ratio gate for unary overhead (≤ 1.10× raw axum).** Rejected (D5): with 0.90 µs bare
  requests the ratio leaves no room for the work RPC must do and measures the denominator.
- **Node + npm + Playwright for the TypeScript client and browser tests (Open question 4).**
  Rejected: it would put a JavaScript toolchain in the repository (L12). Deno and a Rust CDP driver
  do the same with one pinned binary.
- **reqwest's HTTP/3.** Rejected: it needs `--cfg reqwest_unstable`, which would leak into every
  user's build.
- **HTTP-01 or DNS-01 ACME.** Rejected: HTTP-01 needs port 80; DNS-01 needs DNS-provider
  integrations. TLS-ALPN-01 answers on the TLS port already open.
