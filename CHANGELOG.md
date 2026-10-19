# Changelog

## 0.12.0 — unreleased — One call surface

### Added

- `id_effect_axum` features `ws`, `sse`, `tls`, `http3` and `webtransport`, declared once
  (tokio-tungstenite / tungstenite `=0.29.0`, the version of axum's own `ws`; h3 `=0.0.8`,
  h3-quinn `=0.0.10`, h3-webtransport `=0.1.2`; `tls`, `http3` and `webtransport` are filled in
  by their 0.12 leaves). `realtime::ws`: effect-native WebSocket endpoints on typed routes
  (`ws::endpoint(..).public()` / `.guard(g)`), each socket a supervised fiber with a pull-based
  inbound `Stream` and a `WsSender` / `Sink`; a client that goes away interrupts the handler
  (finalizers run); `ws::serve_io` serves an already-upgraded connection. `realtime::sse`:
  `SseEvent` with ids, validated `Last-Event-ID` resume (`SseRequest::last_event_id`),
  heartbeats on the executor clock, `sse::endpoint`, `sse::response`, `sse::encode`.
  `realtime::ConnectionCap` bounds open sockets and streams in total and per client IP. Examples
  `090_ws_echo`, `091_sse_ticker`; book chapter 33.5.
- `id_effect_platform` features `tls`, `quic`, `http3` and `acme`, declared once (ADR 0014 D6;
  rustls 0.23.46+, quinn 0.11.12+ on aws-lc-rs, h3 `=0.0.8`, h3-quinn `=0.0.10`, instant-acme
  0.8). `tls`: certificates, CA bundles, `CertReloader`, `Identity` and the TLS 1.3 rustls configs,
  moved from `id_effect_node::transport::certs`. `quic`: `QuicEndpoint`, one UDP port routed by
  ALPN (`AlpnRouter`, `Route::h3()`, `Route::mesh(..)`), each route with its own certificate.
- `id_effect_platform::tls`: `Identity::install` swaps the certificate of every listener holding
  the identity (the QUIC endpoint and the TCP TLS listener) for new connections; open ones keep
  theirs. Feature `acme` (`tls::acme`): `Acme` issues and renews certificates over ACME
  TLS-ALPN-01 with instant-acme `=0.8.5` (`AcmeConfig`, Let's Encrypt staging by default,
  `.production()`), answers the `acme-tls/1` challenge through `AcmeResolver` /
  `Acme::serve_tls_alpn01` on the TCP TLS listener, renews at two thirds of the lifetime with
  jitter (`Acme::spawn`), and reports `AcmeStats`. Book chapter 29.9.
- `id_effect_node`: `QuicTransport::on_endpoint` puts the mesh (ALPN `idm/1`) on a QUIC endpoint
  shared with HTTP/3; `QuicSettings::mesh_port` (`quic.mesh_port`) keeps it on its own port.
- `id_effect_axum` feature `tls` (ADR 0014 D10): TLS 1.3 and 1.2 on TCP with ALPN `h2` and
  `http/1.1` (`server::tls::ServerTls`, from a platform `Identity`, PEM files polled for
  rotation, any rustls resolver via `ServerTls::builder` with `.tls13_only()`, `.alpn(..)` and
  `.configure(..)` (the hook for `Acme::serve_tls_alpn01`), or `ServerTls::from_rustls_config`;
  connections that negotiate another ALPN, such as `acme-tls/1`, are closed after the
  handshake); HTTP/2 through hyper-util's auto builder. `HostConfig::tls` (`HostTls`;
  `TLS_CERT_FILE`, `TLS_KEY_FILE`, `TLS_RELOAD_SECS`) makes every host entry point serve TLS;
  `server::serve_tls_listener` and `serve_host_listener` / `HostTransport` serve a listener
  directly. A reloaded certificate is used for new connections without a restart.
  `HostConfig::http3` (`HTTP3`, default on) and `HostConfig::posture_warnings`. Book chapter 29.4.
- `id_effect_platform` HTTP/3 client (feature `http3`, opt-in with `HttpClientConfig::http3(true)`):
  `http::Http3Client` on `h3` + `h3-quinn` over its own quinn endpoint (never reqwest's HTTP/3),
  upgrading an `https` origin after its `Alt-Svc: h3` (`http::alt_svc::AltSvcCache`: max-age
  honoured, 256 entries LRU, same-host alternatives only), with TCP fallback when the handshake
  does not complete within 300 ms and a 5-minute negative cache per origin (`Http3Config`);
  pooled connections; `Http3Client::execute_h3` for HTTP/3 without fallback.
  `HttpClientConfig::root_certificates` / `add_root_certificate_der` trusts extra CA roots on
  TCP and HTTP/3. Book chapter 29.8.
- `id_effect_axum` feature `http3` (r12-e12-3-h3-server): `server::h3` feeds `h3` + `h3-quinn`
  requests from the platform `QuicEndpoint` (ALPN `h3`) into the same axum `Router` as a tower
  `Service`, so typed routes, middleware, guards and RPC endpoints work over HTTP/3 unchanged
  (`serve_h3`, `quic_limits`, `Http3Options`). With TLS from certificate files and
  `HostConfig::http3` on (the default), every host entry point binds UDP on the TCP port number
  with the same certificate (reloads reach both), adds `Alt-Svc: h3=":<port>"; ma=86400` to TCP
  responses (`server::alt_svc::AltSvcLayer`) and drains both listeners on shutdown (`GOAWAY` on
  HTTP/3); `HTTP3=false` turns HTTP/3 and `Alt-Svc` off. The `ConnectionLimits` apply to HTTP/3
  (connections, header timeout, requests per connection, QUIC stream credits); the client IP is
  the QUIC peer under the trusted-proxy rules; 0-RTT is off. Example `092_http3`, CI job
  `http3-conformance` (h3spec, curl `--http3-only`), book chapter 29.5.
- `id_effect_platform`: reqwest feature `http2`, so the TCP path (and the `Http3Client`
  fallback) speaks HTTP/2 to servers that offer it (F11).
- `id_effect_platform` QUIC transport tuning (r12-f-h3-platform, ADR 0014 D9): congestion
  control per route (`quic::CongestionControl`, `Route::with_congestion_control`,
  `Route::congestion_control`, `AlpnRouter::congestion_control`): `Route::h3()` runs **BBR**
  (quinn marks it experimental), the mesh (`Route::mesh`, `Route::new`) keeps CUBIC; on a shared
  endpoint a connection switches to its route's algorithm once the handshake has chosen the
  ALPN. `client_config_with` / `transport_config_with` take the algorithm (`client_config` stays
  CUBIC); the `Http3Client` runs BBR. `EndpointStats::congestion_bbr` / `congestion_cubic`. UDP
  socket buffers (F23): the endpoint and the `Http3Client` ask for 4 MiB each way
  (`quic::SocketBuffers`, `QuicEndpointConfig::with_socket_buffers`, `DEFAULT_SOCKET_BUFFER`),
  best effort, granted sizes logged at `debug` and reported by `QuicEndpoint::socket_buffers`
  (raise `net.core.rmem_max` / `wmem_max` on Linux). The `Http3Client` verifies servers with
  `rustls-platform-verifier` (=0.7.1) when no `root_certificates` are configured (F10).
- `id_effect_axum`: `ServerTls::identity`, `ServerTlsBuilder::identity`; `HostTls::Provided`
  settings built with `ServerTls::from_identity` / `from_pem_files` (or given an identity) serve
  HTTP/3 too (F24).
- `id_effect_rpc` is a published crate (tier `integration`, ADR 0010 Amendment 8; no longer
  `publish = false`). New modules: `contract` (the D11 serde model — services, versions,
  procedures, kinds, access decisions, input/output/error JSON Schemas — with
  `Contract::to_json_pretty` / `from_json` and the `assert_snapshot` golden for
  `rpc.contract.json`, `UPDATE_GOLDEN=1`), `wire` (the 0.12 call envelope, `_tag` exit union,
  reserved failures such as `InternalError { id }`, stream frames, `c1.` cursors and 128-bit
  resume session ids), `codec` (`Codec`, `CodecId`, JSON, content-type and WebSocket subprotocol
  negotiation), `server::dispatch` (transport-independent dispatch with per-procedure guards),
  `Procedure` and `RpcGroup::service_contract`. Every 0.12 RPC feature is declared once:
  `client` (default), `postcard`, `ws`, `sse`, `events`, `mesh`, `channels`, `http3`,
  `webtransport`, `test-util`. `id_effect_rpc::service` re-exports `#[rpc::service]` from
  `id_effect_rpc_macros`. Book chapter 33 "One call surface".
- `#[rpc::service(version = N)]` (`id_effect_rpc_macros`, r12-e12-1-macro): a trait whose methods
  carry a kind (`#[query]`, `#[mutation]`, `#[server_stream]`, `#[client_stream]`, `#[bidi]`,
  `#[subscription]`) and an access decision (`#[public]` or `#[guard(G)]`, required — ADR 0013
  R1) becomes the handler trait, `<Trait>Service` (`contract()`, `register`, `group`, `server`,
  `mount`) and `<Trait>Client<T>` over any `service::RpcTransport` (`LocalTransport` for every
  kind in-process, `HttpTransport` for queries and mutations over `POST /rpc`); calls fail with
  `RpcCallError<E>`. `#[derive(RpcErrorUnion)]` makes an enum a typed error union
  (`#[rpc(status = N)]`, `#[rpc(tag = "…")]`, `#[rpc(internal)]`) and refuses the reserved
  failure tags. Versions are served side by side (`name = "…"` shares a service name). Input,
  output and error field types are the built-ins or `#[derive(Schema)]` types
  (`service::RpcSchema`); types that also implement serde get `WireSchema::with_serde`, so the
  generated procedures are served over postcard; misuse fails with a `help:` line (`tests/ui`).
  The contract snapshot `crates/id_effect_rpc/rpc.contract.json` is checked by
  `contract_snapshot_is_current`. Book chapter 33.4.
- `id_effect_rpc` procedure kinds (ADR 0014 §2, r12-e12-1-kinds): `Procedure::server_stream`,
  `client_stream`, `bidi`, `subscription` and `.version(n)`, `.max_items(n)`, `.max_bytes(n)`;
  `RpcGroup::{query, mutation, server_stream, client_stream, bidi, subscription}` take the
  handler shapes of the new `handler` module (`QueryHandler`, `MutationHandler`,
  `ServerStreamHandler`, `ClientStreamHandler`, `BidiHandler`, `SubscriptionHandler`, each a
  method returning an `Effect`) and `WireSchema` (from any `Schema`, `WireSchema::of::<T>()`).
  `kinds`: `Duplex`, `Subscription`, `SubscriptionItem`, `CallFrames`, and the in-process
  transport `InProcess` (feature `test-util`). `error_union`: typed error unions
  (`RpcErrorUnion`, `RpcErrorStatus`). `cancel`: per-call deadlines (`x-rpc-deadline-ms`, capped
  by `RpcServer::with_max_deadline`, 30 s default) and cancel signals. `idempotency`:
  `IdempotencyStore` and the bounded `MemoryIdempotencyStore` (24 h, 100,000 keys, D14).
  `server::dispatch`: `open` and `start_*` for every kind, the seam of every transport. HTTP:
  `GET /rpc/{tag}?input=` for queries with ETag / 304, `POST /rpc/{tag}` for every unary and
  stream kind (NDJSON server streams, NDJSON client streams with item and byte caps). Book
  chapter 33.1.
- `id_effect_rpc::subscription` (r12-e12-2-subscriptions): `TailConfig` (read batch 1–1,000,
  poll interval 10 ms–60 s, clamped), `position_after` and the typed `CursorMismatch` failure
  (`{"_tag": "CursorMismatch"}`, 400). Feature `events`: `subscription::events::EventSubscription`
  serves an `id_effect_events` `EventLog` as a resumable subscription — the cursor is the global
  position (`Cursor::events`), a call replays from it and then tails the log in one loop (no gap
  or duplicate at the hand-over or on resume), a projection filters events, a slow subscriber is
  lagged (pull-based, at most the window plus one batch in memory), interrupting the call
  releases the reader (`open_readers`), and a failed read is `InternalError { id }`. Book chapter
  33.8.
- `id_effect_rpc` feature `postcard` (r12-e12-1-postcard, D13; postcard `=1.1.3`):
  `CodecId::Postcard` (`application/vnd.id-effect.rpc+postcard`, `idrpc.postcard.v1`) and
  `codec::postcard` — the postcard `Frame` (fixed `FrameKind` indices 0–9), call, result and exit
  envelopes, u32 big-endian length-prefixed records, and the transport bridges
  `decode_client_payload` / `encode_server_frame`. `WireSchema::with_serde` adds a typed bridge
  (`codec::TypedCodec`) and `RpcMethodEntry::codecs` (`ProcedureCodecs`) carries it: postcard
  decodes the type with serde, then the Schema's validators run as for JSON. `codec::negotiate_accept`
  picks the response codec. Book chapter 33.3.
- `id_effect_rpc::openapi` emits **OpenAPI 3.1** from the contract (`openapi::document`,
  `to_json`, `to_yaml`, `OpenApiOptions`; web.rpc #7, r12-e12-4-openapi-asyncapi): queries as
  `GET ?input=` (ETag, 304) and `POST`, mutations as `POST` with `Idempotency-Key`, server streams
  as NDJSON frames, client streams as NDJSON items, the envelope and batch route `POST /rpc` with
  every `(tag, version)`, the `x-rpc`, `x-rpc-version` and `x-rpc-deadline-ms` headers,
  `WireResult` responses per status and RFC 9457 Problem responses; error unions are `oneOf`
  with a `_tag` discriminator. `id_effect_rpc::asyncapi` emits **AsyncAPI 3.0** for server and
  client streams, two-way streams, subscriptions and `ChannelTopic`s over WebSocket (subprotocol
  binding) and SSE, with the cursor documented. Contract `$defs` become components with
  deterministic names. `openapi::ContractDocs` serves `/rpc/openapi.json`, `/rpc/asyncapi.json`
  and `/rpc/_contract`, not mounted outside `dev` and `test` unless guarded (ADR 0013 R7;
  `public_dangerous` is recorded). Book chapter 33.10.
- `id-effect rpc gen ts --contract <rpc.contract.json> --out <dir>` (web.rpc #3, web.realtime
  #7, r12-e12-4-gen-ts): a typed TypeScript client from the service contract, plain TypeScript
  with no runtime dependency (`client.ts`, `types.ts`, `runtime.ts`; deterministic). Types from
  the contract: structs, tagged unions, `Option` as optional / `| null`, `Vec`, maps, newtypes as
  branded types, dates as ISO strings, `$defs` named as the OpenAPI components; error unions
  discriminated on `_tag`; one class per service and version. The runtime batches the queries
  and mutations of one microtask (≤ 50 calls, 1 MiB), sends `x-rpc: 1` always and a 128-bit
  `Idempotency-Key` with every mutation (`x-rpc-ordered: true` for more than one), streams over
  WebTransport, then WebSocket, then SSE + POST, validates every server frame, reconnects with
  exponential backoff and jitter, resumes server and two-way streams from their last cursor and
  reopens subscriptions after theirs, and cancels on `AbortSignal`. Library:
  `id_effect_tool::rpc::gen_ts::{generate, generate_to}`. Tests run under Deno (pinned 2.7.9 in
  devenv and CI job `ts-client`). Book chapter 33.11.
- `id_effect_rpc` batches (design note §2.2, r12-e12-1-batching): `POST /rpc` with a JSON array
  of calls answers an array of results in request order, each call failing on its own; queries
  and mutations only (other kinds and unknown tags are that call's `InvalidInput`). At most 50
  calls and 1 MiB (`batch::BatchConfig`, `RpcServer::with_batch_config`; over is a `413` Problem
  before any element is decoded, `wire::decode_batch` for the fuzz target); every call's guards
  run before any call does; bounded parallelism through a compute `AdmissionController` (8 by
  default), or in order with `x-rpc-ordered: true`; one `RequestCache` per batch, so duplicate
  loads through a `RequestResolver` are merged; mutations keyed `<Idempotency-Key>#<call id>`.
  `RpcClient::call` sends the calls of one tick as one batch (`call_unbatched` opts out);
  `CallContext::with_request_cache`. `id_effect_axum`: `RateLimitLayer` leaves a
  `middleware::rate_limit::RateLimitCharge` in the request extensions so a handler charges the
  rest of a request's cost (`take(n)`, all or none on `MemoryRateLimitStore`; a batch of N costs
  N tokens); `RateLimitStore::{acquire_n, try_acquire_n_now}` with defaults. Book chapter 33.2.
  Postcard over plain HTTP (feature `postcard`): queries and mutations take a postcard call
  envelope on `POST /rpc` and postcard input bytes on `POST /rpc/{tag}`, answered with a
  postcard result through the procedure's typed codec (`415` without `WireSchema::with_serde`,
  and for postcard server streams; batches stay JSON); `RpcClient::call_postcard`,
  `client::http::decode_postcard_response`.
- `id_effect_rpc::channels` (feature `channels`, r12-e12-2-channels; ADR 0014 D3: a module, no
  `id_effect_channels` crate): topics with join and leave, presence and broadcast across the
  mesh. One hub process per node (`register_hub`, `Channels::builder().start_on_mesh(&mesh)`)
  registered cluster-wide; a topic is a mesh `ProcessGroups` group of the interested hubs, so a
  publish is one cast per other interested node. `Channels::join` / `serve_joins` (a
  `subscription` procedure: input `JoinInput`, items `ChannelEvent` — `joined` snapshot, `message`,
  `presence` diffs), `Channels::publish` (per-publisher order: duplicates and late arrivals are
  dropped), `Presence` with per-connection metadata, heartbeat (`heartbeat`, `spawn_heartbeat`)
  and monitor-driven cleanup when a node goes down, `ManualClock` / `SystemClock`. The cross-node
  seam is `Backplane` (`MeshBackplane` on process groups and casts; `ChannelsBuilder::start_with`
  for another transport); `attach_session` runs a mesh over an authenticated session and
  `connect_in_memory` builds an in-memory mesh. Example `channels_chat`; book chapter 33.9.
  Feature `channels` adds the dependency `id_effect_process` (the hub is a process).
- `id_effect_rpc` feature `mesh` (r12-e12-2-mesh-rpc): the same `RpcGroup` callable node to
  node over the process mesh, postcard always. `transport::mesh`: `MeshRpc` per node (register
  `MeshRpc::register_behavior` before building the `Mesh`), `MeshRpc::serve(..)` →
  `MeshEndpoint`, one endpoint process per service under the cluster-wide name `rpc:<service>`
  (`start`, or `child_spec` under a supervisor: a restart registers the name again);
  `MeshPeer`, `MeshPeerGuard`. `client::mesh`: `MeshRpc::client(service)` → `MeshRpcClient`
  with every kind as effects (`query`, `mutation`, `server_stream`, `subscribe`,
  `client_stream`, `bidi`; `MeshStream`, `MeshSink`, `MeshReply`), credits both ways, the
  caller's span as the parent of the endpoint's `rpc.request` span, typed `MeshRpcError`;
  `MeshTransport`, the `RpcTransport` of `#[rpc::service]` clients over the mesh. Book chapter
  38.2.
- `id_effect_rpc` transports (r12-e12-2-rpc-ws-sse, design note §5): feature `ws` serves every
  kind over one WebSocket at `GET /rpc/ws` (`RpcServer::with_websocket(RpcWsConfig)`, on
  `id_effect_axum`'s realtime layer; subprotocols `idrpc.json.v1` and, with `postcard`,
  `idrpc.postcard.v1`), frames multiplexed by call id with credit backpressure both ways, a `hello`
  frame carrying the connection's session (`transport::ws::WsSession`, bound to the principal),
  `transport::ws::serve_socket` for an already-upgraded io. Feature `sse` serves subscriptions and
  server streams as server-sent events on `GET /rpc/{tag}` with `Accept: text/event-stream`
  (cursor = event id, `Last-Event-ID` resume; `RpcServer::with_sse`). `transport::frame::decode`
  / `encode` / `validate` is the frame codec (fuzz entry point). Client: `RpcClientConfig::transport`
  (`TransportKind::{Http, WebSocket, Sse}`), `RpcClient::subscribe` over the configured transport
  (`RpcStream`, `CallEvent`), `client::ws::WsClient` / `WsCall`, `client::sse` (`RpcClient::sse`,
  `SseCall`). Generated clients reach the network streams through `service::RpcTransport`:
  `client::ws::WsClient` carries every kind, `client::sse::SseTransport` subscriptions and server
  streams (queries and mutations over `POST /rpc`). A call refused before it ran exits with the
  `Rejected` failure on frame transports. Every call on a WebSocket, and every SSE stream, runs
  with the request's capabilities (an `Authenticate` layer's `CurrentPrincipal`) and a
  `transport::ConnectionId` (one per socket; `channels::ConnectionId` is this type). Book chapter
  33.6.
- `id_effect_rpc` two-way streams with backpressure and resume (r12-e12-2-bidi-resume, web.rpc#4):
  `Duplex` over WebSocket with credits in both directions (a stream's memory is bounded by the
  window). Server streams and two-way streams on a WebSocket are resumable: their `item`s carry
  `Cursor::sequence(n)` and stay in a per-session replay buffer until credited; a dropped socket
  detaches them, and a `resume` on a new socket replays after each call's cursor (answered by a
  `hello` naming the connection's session) or exits the call `ResumeExpired`. Module `resume`:
  `ResumeConfig` (1,024 messages / 1 MiB / 30 s per session, 1,024 detached sessions),
  `ResumeStore`, `ResumeStats`, `ResumeError`, `TestClock`, the bounded decoders
  `decode_cursor` and `decode_resume` (fuzz targets). `RpcWsConfig::resume`, `resume_stats`,
  `release_expired_sessions`. Book chapter 33.7.

### Changed

- `id_effect_rpc::openapi` emits OpenAPI 3.1 from a `Contract` instead of 3.0 from route
  metadata: `OpenApiSpec`, `OpenApiInfo`, `RpcServiceDef`, `RpcMethodDef`, `RpcHttpMethod`,
  `emit_openapi_json` and `emit_openapi_yaml` are removed (see the migration guide).
- `id_effect_node::transport::certs` is a deprecated re-export of `id_effect_platform::tls` for
  one minor. The QUIC mesh requires ALPN `idm/1`: 0.11 nodes cannot join a 0.12 QUIC mesh. See
  the [0.12 migration guide](docs/MIGRATION-0.12.md).
- `id_effect_rpc`: `RpcGroup::register` takes a `Procedure` with an access decision
  (`Procedure::query(tag).public()` / `.guard(g)`) and no longer takes input field names; tags
  are validated and registered once. `RpcServer` mounts `POST /rpc` as a declared route of
  `id_effect_axum::route::Router` (`mount`, `router`, `endpoint`; `layer_rpc` takes the typed
  router) and caps bodies (1 MiB default). `RpcClient::call` no longer takes output field names;
  `RpcClientError::Rpc` carries the `RpcEnvelope`. The `codegen` module (trait source strings) is
  removed; its route metadata types moved to `openapi`. `RpcClient` is behind the default
  `client` feature.
- `id_effect_rpc` speaks the 0.12 wire format on HTTP (`WireCall` in, `WireResult` with a `_tag`
  exit out); the labs `RpcWireRequest` / `RpcWireResponse`, the `stream` module and the
  serialization helpers built on them are removed. A typed failure answers its error's status, a
  Schema failure 422 `InvalidInput` with every issue. Every `POST` needs a codec `Content-Type`
  and `x-rpc: 1`. `RpcClientError::Rpc` is replaced by `Failure` (the `_tag` value) and `Refused`
  (the server's Problem). `RpcServer::endpoint` is `endpoints` (three routes); `RpcMethodEntry`
  no longer has `dispatch` (use `server::dispatch`).
- `id_effect_rpc::RpcClientConfig` gains the public field `transport` (struct literals need
  `transport: TransportKind::Http` or `RpcClientConfig::new`), and `RpcClientError` the variant
  `Transport`. With feature `sse`, `GET /rpc/{tag}` with `Accept: text/event-stream` on a
  subscription or server stream answers an event stream instead of 406 / 405.
- `id_effect_platform::http::HttpClientConfig` gains the public fields `root_certificates` and
  (feature `http3`) `http3`: struct literals need `..HttpClientConfig::default()`.
- `id_effect_axum::HostConfig` gains the public fields `tls` and `http3` (struct literals need
  `..HostConfig::default()`); `ConnectionLimits::max_requests_per_connection` also applies to
  HTTP/2 (a `GOAWAY` after the n-th request).

### Security

- RPC resume (r12-e12-2-bidi-resume): a resume session is 128 random bits bound to the
  connection's principal; a resume under another principal is refused like an expired buffer
  (`ResumeExpired`, indistinguishable from an unknown session) and counted. A cursor is not a
  capability: every resumed call runs its procedure's guards again on the new connection. Every
  replay buffer and the set of detached sessions are bounded; a detached session is released
  after the retention and its calls are interrupted. `Rejected` joins the reserved failure tags
  (`RpcFailure::RESERVED_TAGS`): a user error variant can no longer claim it (refused by
  `#[derive(RpcErrorUnion)]`, `InternalError` at run time).
- Mesh RPC (r12-e12-2-mesh-rpc): a call runs only on an origin the receiving node verified —
  inbound mesh traffic goes through `MeshRpc::ingress` / `MeshRpc::handle_session_frame`, which
  stamps messages for RPC processes with the authenticated session's node under a per-node
  256-bit key; anything else is dropped. The calling node is the principal (`MeshPeer`,
  `RpcPrincipal("mesh:<node>")`); end-user rights never cross the hop. Calls per peer (256) and
  per endpoint (4,096) are capped before guards run, deadlines are clamped (30 s), items beyond
  the granted credits and client streams over their caps are `ResourceExhausted`; a call is
  dropped when the caller cancels, its node or reply process goes down, or the endpoint exits.
  Queries are retried on a gone endpoint (3 times), mutations never.
- RPC transports (C3, C4, C8): the WebSocket route refuses an upgrade offering no rpc subprotocol
  (415), keeps the realtime layer's origin allowlist (empty: every browser origin refused),
  connection caps, 64 KiB frames, message rate, heartbeats and idle timeout, and caps open calls per
  connection (100, `RpcWsConfig::max_calls`; beyond: `ResourceExhausted { limit: "calls" }`). Each
  call runs its procedure's guards on the upgrade (or `GET`) request head. A peer that sends past its
  credits fails that call (`ResourceExhausted { limit: "credits" }`); undecodable frames close the
  socket. Session ids are 128-bit random and bound to the connection's principal.
- RPC codecs (D13, C3): an unknown codec is 415, never a JSON fallback; a procedure without a
  typed bridge refuses postcard (415). Every postcard decoder refuses input over 64 KiB before
  decoding, limits nesting to 64, bounds the elements of all sequences and maps by the input size,
  never preallocates past the input, and refuses trailing bytes, unknown frame kinds, reserved
  flag bits and invalid tags (`codec::postcard::decode_frame` is the fuzz entry point).
- TLS listener defaults (ADR 0013 R4; ADR 0014 D10, C3): a TLS handshake must finish within 10 s
  (`ConnectionLimits::tls_handshake_timeout`; `dangerous_no_tls_handshake_timeout` is recorded),
  handshakes count against the connection cap, HTTP/2 allows 100 concurrent streams per
  connection (`max_concurrent_streams`; `dangerous_unlimited_concurrent_streams` is recorded) and
  pings dead peers, 0-RTT is off, and plain HTTP is not answered on the TLS port. A `HostConfig`
  with `tls` set refuses to start when the crate is built without feature `tls` instead of
  serving plain HTTP; HTTP/3 enabled without TLS is a posture warning (`warn` in prod).
- ACME (ADR 0014 D8): a failed order keeps the current certificate (logged, counted in
  `AcmeStats::failed`, retried); the account key and the certificate key are stored with mode
  0600 in a 0700 cache directory (looser modes are tightened), held as `id_effect_config::Secret`
  in memory and never printed; the directory must be `https://`; DNS-01, HTTP-01 and wildcard
  names are not offered.
- HTTP/3 client (ADR 0014 C15): the 0.11 SSRF policy checks the URL and every resolved address
  before a UDP packet is sent, and the connection uses the checked address; the total timeout
  covers the whole exchange and the body cap holds on a declared `Content-Length` and while
  reading; TLS 1.3 only, 0-RTT off, the server name verified against the platform CA bundle; an
  `Alt-Svc` naming another host is ignored; a `POST` or `PATCH` that may have reached the server
  is not resent over TCP. A test fails the build if reqwest's unstable HTTP/3 cfg appears in a
  manifest, source, workflow or Nix file.
- HTTP/3 server (ADR 0013 R1 R4 R10; ADR 0014 C3): the TCP `ConnectionLimits` bound HTTP/3 too
  (an HTTP/3 connection pool the size of `max_connections`, the header-read timeout on the first
  request and on every request's header section, `GOAWAY` after `max_requests_per_connection`,
  QUIC stream credits at `max_concurrent_streams` capped at the endpoint ceiling of 100, the TLS
  handshake timeout on the QUIC handshake); request header sections are capped at 64 KiB;
  connection-specific response headers are dropped; the client IP comes from the QUIC peer
  address through the trusted-proxy rules; 0-RTT is off (the `Http3Options` opt-out
  `dangerous_allow_zero_rtt_for_queries` is recorded and admits safe methods only); an h3 stream
  is never `stop_sending`'d while a read is in flight (F13: it is dropped).
- WebSocket and SSE defaults (ADR 0013 R4, R9; ADR 0014 C3, C4): the `Origin` of every upgrade
  is checked against an allowlist that is empty by default (every browser origin refused, 403;
  the dev profile allows `realtime::origin::DEV_ORIGINS` by configuration);
  `WsConfig::dangerous_any_origin` is recorded in the posture registry. Frames and messages are
  capped at 64 KiB (close 1009), inbound messages at 100 per second per socket (close 1008),
  sockets and SSE streams at 16 per client IP and 10 000 per endpoint (429 before the upgrade);
  a ping every 20 s with two missed pongs closing the socket and a 300 s idle timeout (1001);
  bounded inbox and outbox. Tests `r9_ws_upgrade_from_foreign_origin_refused`,
  `dangerous_any_origin_is_recorded`, `r4_ws_frame_over_max_closes_1009`,
  `r4_ws_sockets_per_ip_capped`, `heartbeat_closes_dead_connection`.
- Shared QUIC endpoint (ADR 0014 D7): with `h3` and `idm/1` on one port the handshake requests
  client certificates as optional with no CA hints and still verifies a presented one against
  the cluster trust; the `idm/1` route closes every connection without a verified client chain
  before any frame of it is read (`CLOSE_CLIENT_CERT_REQUIRED`). Tests
  `idm_without_client_cert_refused_on_shared_endpoint`,
  `idm_cert_from_other_ca_refused_on_shared_endpoint`; the 0.11 mutual-TLS tests run on both
  shapes.
- RPC access (ADR 0014 C1, finding H8 at the call surface): a procedure without `.public()` or
  `.guard(g)` does not register (compile error); its guards run on the request head before the
  input is decoded; an unknown tag answers 404 Problem Details; the `/rpc` mount sits behind the
  deny-by-default router. Over-cap bodies are 413 and unknown codecs 415 Problems. Tests
  `rpc_unregistered_tag_is_404_problem`, `rpc_group_requires_access_decision`.
- RPC calls (ADR 0014 C5, C10; ADR 0013 R3, R4, R9): every `POST` needs `application/json` (or
  the stream's NDJSON) and `x-rpc: 1` (415 / 403 otherwise), so a form or a simple cross-site
  request cannot call a procedure; a defect, a panic or an error that claims a reserved tag
  answers `InternalError { id }` with the correlation id only (the cause is logged); a client
  disconnect or an expired deadline interrupts the handler fiber and runs its finalizers; client
  streams are capped (10,000 items and 4 MiB by default, 64 KiB per line); `Idempotency-Key`
  results are scoped by principal, and for anonymous callers by client IP with keys of at least
  16 characters. Tests `r9_rpc_mutation_without_custom_header_refused`,
  `r9_rpc_form_content_type_refused`, `defect_becomes_internal_error_without_message`,
  `client_disconnect_interrupts_handler_and_runs_finalizers`, `deadline_capped_by_server_max`,
  `client_stream_over_cap_rejected`, `idempotency_replay_is_scoped_for_anonymous_callers`.
- QUIC endpoint defaults (ADR 0013 R4, R8): address validation (Retry) on, 0-RTT off, finite
  stream credits, idle timeout, UDP payload, receive windows, pending handshakes (256) and
  handshake time (10 s); datagrams off. Opt-outs `QuicEndpointConfig::dangerous_no_retry` and
  `dangerous_raise_limits` are recorded in the posture registry. Tests
  `r4_quic_retry_on_by_default`, `r4_quic_limits_finite_by_default`.
- Channel joins (ADR 0013 R1, ADR 0014 C11): every join, subscribe and resume goes through a
  `JoinPolicy`; the default `DenyAllJoins` refuses everyone (`{"_tag": "Unauthenticated"}` 401 /
  `{"_tag": "JoinDenied"}` 403, the reason only in the log); `AuthzJoin` runs an
  `id_effect_authz` policy (no axum items) with the tenant check; `dangerous_allow_all_joins` is
  recorded in the posture registry. Bounded by default: 10,000 members per topic, 32 topics per
  connection, 10,000 topics per node, 1 KiB join metadata, 16 KiB messages (60 KiB at most), 256
  buffered events per subscriber (a slower one is dropped with `Lagged`); remote members lapse
  15 s after their node's last heartbeat. Tests `channel_join_denied_by_default_policy`,
  `caps_bound_members_topics_and_payloads`, `slow_subscriber_is_dropped_as_lagged`,
  `three_node_broadcast_over_quic_mtls` (mutual TLS, an unpinned node refused).
- Call-surface proof (r12-e12-5-fuzz-security): six new cargo-fuzz targets with seed corpora,
  `rpc_wire_request`, `rpc_batch`, `rpc_postcard_frame` (every postcard decoder, a recursive type
  and a 2^40-element `Vec<()>` among the seeds), `rpc_duplex_frame` (`transport::frame::decode`,
  JSON and postcard, resume payloads), `rpc_resume_cursor` and `sse_last_event_id` (17 targets;
  the ClusterFuzzLite PR and batch budgets grow so each target keeps 5 and 10 minutes).
  Compile-fail cases `r1_rpc_procedure_without_access_decision` and `r10_zero_rtt_on_mutation`
  (19 cases).
  `id_effect_rpc/tests/security_surface.rs` fails when a test the release spec's Security table
  names is missing, lists every 0.12 `dangerous_*` opt-out and checks each records itself, and runs
  the Origin, `x-rpc`, batch-cap, batch-refusal (`x-rpc-batch-index`) and call-cap cases against
  one server. THREAT-MODEL.md gains the call-surface boundary B9 and its residual risks (HTTP/2
  idle connections, F14). The `#[rpc::service]` UI suite now also runs with feature `channels`
  (F31: `ChannelError`'s `RpcErrorUnion` impl is `#[diagnostic::do_not_recommend]`).

## 0.11.0 — 2026-10-19 — Distribution & hardening

The secure-by-default release (ADR 0013): every finding of the 2026-10-18 security review is
closed in code, each with a regression test, and every way back to the old behaviour is a named
`dangerous_*` / `insecure_*` opt-out that records itself in the posture registry and fails the
new prod posture check unless the risk is accepted. Also: mutual TLS on the mesh, protocol v3
with opt-in at-least-once delivery, durable workflows (`id_effect_workflow`), STM in core,
`Statem`, process timers and hibernate, Dylint lints, and supply-chain hardening. Upgrading from
0.10: see the [0.11 migration guide](docs/MIGRATION-0.11.md); a 0.11 mesh node refuses 0.10.x
peers.

### Security

- `id_effect_axum` typed routes are deny by default (H8, ADR 0013 R1): `route::Router::route`
  only compiles for an endpoint whose route declared `.public()` or `.guard(g)` (new
  `RouteGuard` trait, implemented by `id_effect_authz::Guard`, which on a typed route answers 401
  to the anonymous principal before its policy). `RouteInfo::access` records the decision,
  OpenAPI documents public operations with `security: []`, `Router::access_report()` lists the
  public routes and `into_axum()` logs it at startup and installs a deny-by-default layer that
  answers 404 Problem Details for unregistered routes (`Router::merge_axum`; opt-out
  `allow_unregistered_routes_dangerous()`, recorded). `Router::route_unchecked` is the deprecated
  one-minor shim, recorded in the posture registry. See "Routing: every route is public or
  guarded (H8)" in the [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_axum` `SecurityLayer` (L1, L2, L5, M4): the CSRF token is masked per response
  (one-time pad, BREACH) and unmasked before the constant-time compare; `CsrfToken::rotate`
  replaces the session secret (call it at login); the CSRF form read is bounded by
  `CsrfConfig::form_limit` (64 KiB, was 2 MiB) and `read_timeout` (10 s) whatever the layer
  order; `SecurityHeaders::recommended()` adds `Cross-Origin-Opener-Policy`,
  `Cross-Origin-Resource-Policy` and a deny-list `Permissions-Policy`, each overridable. See
  "Web: CSRF masked token format" in the [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_axum` (M5): new `middleware::LoadShedLayer` caps the requests in flight and answers
  `503` Problem Details with `Retry-After` at once when full (no queue). Every server entry point
  (`server_layer`, `serve*`, `serve_router*`, `ServerHandle::start*`) installs it from the new
  `HostConfig::load_shed` (default 512 per available CPU; env `MAX_IN_FLIGHT`). Opt-out:
  `HostConfig::dangerous_no_load_shedding()`, recorded in the posture registry.
- `id_effect_axum` (M8): `MemoryRateLimitStore` is sharded by key hash (`shards`, default four per
  CPU, at most 64) and each shard is an LRU list, so a lookup and an eviction are O(1) instead of
  a scan of every key under one mutex. `DEFAULT_MAX_KEYS` stays 100,000. New provided method
  `RateLimitStore::try_acquire_now`: the in-memory store answers inline and `RateLimitLayer`
  skips the executor round trip for it.
- Operator surfaces closed in prod (M6, M7, L2, M4 template; ADR 0013 R7): `id_effect_axum`
  `OpenApiUi` loads Scalar from an exact version (`@scalar/api-reference@1.75.0`) with its SRI
  `sha384` hash by default (`DEFAULT_SCALAR_INTEGRITY`) and mounts `/openapi.json` and `/docs`
  outside the `dev`/`test` profiles (`OpenApiUi::profile`, else `ID_EFFECT_PROFILE`) only when
  guarded (`OpenApiUi::guard`) or `public_dangerous()` (recorded); new
  `metrics::guarded_metrics_router`. `id_effect_console` `ConsoleConfig` carries the allowed
  hosts (default: the bind address) and `check_host_and_origin` answers `403` to another `Host`
  or `Origin` (DNS rebinding); `ConsoleConfig` is no longer `Copy`. The service template guards
  `/metrics` with a bearer `METRICS_TOKEN`, wraps `SecurityLayer` in the body-limit and timeout
  layers, keeps the console's local operator to the `dev` profile and checks its Host and Origin
  (`CONSOLE_ALLOWED_HOSTS`). The Ledger enables the console in `dev.toml` and `test.toml` only.
  See "Web: `/docs`, `/metrics` and the console are closed in prod" in the
  [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_node` QUIC mutual TLS (dist.tls #4 #5 #7, ADR 0013 R8): every dial presents the
  node's certificate and the listener verifies it (CA roots via `with_client_verification`, or
  `with_pinned_client_cert_sha256` / `with_pinned_peer_cert_sha256`); a peer without a client
  certificate or from another CA is refused, and `listen` without client trust fails with
  `QuicConfigError::NoClientTrust`. New `transport::certs`: `NodeCertificate::from_pem_files`
  (PKCS#8, PKCS#1, SEC1 keys), `load_ca_bundle`, and `CertReloader`, which polls the files so new
  connections use a rotated certificate without a restart (`QuicConfig::from_reloader`).
  Opt-outs `QuicConfig::dangerous_no_client_auth()` and `dangerous_accept_any_server_cert()` are
  recorded in the posture registry. New features `tls-tcp`, `insecure-plaintext-tcp`,
  `kubernetes`. Book "Transport security: mutual TLS", example `134_mtls_cluster`. See "Node: QUIC
  requires client certificates" in the [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_node` TCP is encrypted by default (dist.tls #2 #4, ADR 0013 R8): `TcpTransport::tls`
  (feature `tls-tcp`) carries the frames in TLS 1.3 with mutual auth, configured by the new
  `transport::tls_tcp::TlsTcpConfig` from the same `transport::certs` material and trust options
  as QUIC (CA roots or pins, `from_reloader` for rotation, opt-outs recorded as
  `id_effect_node::TlsTcpConfig::dangerous_*`), with a TLS channel binding for the cookie proof
  and background handshakes bounded by a timeout. Plaintext TCP exists only with the feature
  `insecure-plaintext-tcp`, as `TcpTransport::dangerous_plaintext()`, recorded in the posture
  registry; `TcpTransport` is no longer a unit struct. `Cookie::dangerous_allow_short()` is now
  recorded too (`id_effect_node::Cookie::dangerous_allow_short`). See "Node: plaintext TCP needs
  the `insecure-plaintext-tcp` feature" in the [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_auth` security state shared by every instance (M18, H5 follow-up, ADR 0013 R10):
  `totp::PgTotpStepStore` (features `pg` + `totp`) records consumed TOTP steps with one atomic
  compare-and-set upsert, so a code accepted on one instance is a replay on every other;
  `PgThrottleStore` (feature `pg`) keeps the `LoginThrottle` and TOTP-lockout failure counts in
  PostgreSQL (one upsert per failure, `purge_forgotten` for old keys). Both tables come from the
  new migration `0003_id_effect_auth_state` (or either store's `ensure_schema`). Creating a
  `MemoryTotpStepStore` now records `id_effect_auth::MemoryTotpStepStore::memory_only` in the
  posture registry, so the prod posture check reports memory-only TOTP replay state.
- `id_effect_auth` (L5): `Sessions::login_with_csrf` (feature `axum`) logs in under a fresh
  session id and then rotates the request's `CsrfToken`, so no CSRF token rendered before login
  validates after it. See "Auth: shared security state and rotation at login" in the
  [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_axum` `ConnectionLimits` (M1 follow-up, ADR 0013 R10):
  `dangerous_no_header_read_timeout`, `dangerous_unlimited_connections` and
  `dangerous_unlimited_requests_per_connection` are now recorded in the posture registry, under
  the new `server::DANGEROUS_*` ids.
- `id_effect_platform` HTTP client SSRF guard (M3, ADR 0013 R6): `HttpClientConfig::default()`
  (and so `ReqwestHttpClientProvider::layer()`, `ReqwestHttpClient::default_client()`,
  `with_timeouts` and the new `from_config`) carries an `http::SsrfPolicy` that refuses
  loopback, private, CGNAT, link-local (cloud metadata), unique-local, multicast, reserved and
  IPv4-mapped destinations, on literal-IP URLs and on every address a host name resolves to (a
  custom reqwest DNS resolver, so DNS rebinding is covered); schemes other than http/https are
  refused. Redirects are no longer followed; `follow_redirects(max)` opts in and re-checks every
  hop. Exemptions `allow_hosts` / `allow_cidrs` (`IpCidr`), deserializable from config; opt-out
  `dangerous_allow_private_networks()`, recorded in the posture registry. Refusals are
  `HttpError::InvalidRequest` and are not retried. The Ledger reads its allowlist from
  `[outbound]`. See "HTTP client: SSRF guard on by default" in the
  [0.11 migration guide](docs/MIGRATION-0.11.md).
- The remaining outbound clients follow M3 (ADR 0013 R6, R10): `id_effect_platform`
  `ReqwestClientLive::layer()` and `provide_reqwest_pool` build their `reqwest::Client` from
  `HttpClientConfig::default()` (timeouts, no redirects, the SSRF check in the DNS resolver;
  previously `Client::new()`), with new `layer_with(config)`, `provide_reqwest_pool_with` and
  `ReqwestClient::from_config` / `PooledClient::from_config`. Caller-built clients go in through
  the new recorded opt-outs `dangerous_provide_reqwest_client` and
  `ReqwestHttpClient::dangerous_unguarded`; `provide_reqwest_client` and `ReqwestHttpClient::new`
  are deprecated shims for them. `id_effect_auth` OIDC requires `https` for the issuer and the
  published token, user-info and JWKS endpoints (plain `http` to loopback only, or after the
  recorded `OidcConfig::insecure_allow_http()`), adds a connect timeout
  (`oidc::DEFAULT_CONNECT_TIMEOUT`), and pins with regression tests that a discovery or token
  endpoint answering `302`/`307` is not followed.
- The SSRF guard now also covers literal-IP URLs on the `reqwest::Client` the
  `id_effect_platform::http::reqwest` helpers hand out (M3): every client
  `ReqwestHttpClient::from_config` builds (and so `ReqwestClientLive::layer`,
  `provide_reqwest_pool`, `ReqwestClient::from_config`, `PooledClient::from_config`) checks the
  destination in its connector, so `http://169.254.169.254/` or `http://[::1]/` is refused before
  connecting unless `allow_cidrs` / `allow_hosts` list it (previously only `http::execute`
  checked literal IPs; reqwest skips the resolver for them). `id_effect_auth`
  `JwksClient::insecure_allow_http()` replaces `allow_http_dangerous()` (now a deprecated shim)
  and records `jwks::INSECURE_ALLOW_HTTP` in the posture registry (ADR 0013 R10).
- The 0.10.1 `id_effect_auth` opt-outs now record themselves in the posture registry (ADR 0013
  R10), so the prod posture check refuses them unless the risk is accepted:
  `BasicAuth::dangerous_without_throttle()` (H5, `basic::DANGEROUS_WITHOUT_THROTTLE`),
  `Totp::dangerous_without_lockout()` (H6, `totp::DANGEROUS_WITHOUT_LOCKOUT`) and
  `JwtVerifier::dangerous_any_issuer()` / `dangerous_any_audience()` (H7,
  `jwt::DANGEROUS_ANY_ISSUER` / `jwt::DANGEROUS_ANY_AUDIENCE`). They replace
  `without_throttle_dangerous()`, `without_lockout_dangerous()`, `any_issuer_dangerous()` and
  `any_audience_dangerous()`, now deprecated shims that record the same ids.
- The CSRF and redaction opt-outs record themselves (ADR 0013 R10): `id_effect_axum`
  `SecurityLayer::dangerous_without_csrf()` records `server::security::DANGEROUS_WITHOUT_CSRF`
  (R9) and `id_effect_logger` `RedactionPolicy::dangerous_empty()` records
  `id_effect_logger::DANGEROUS_EMPTY` (M11); the prod posture probes "CSRF on" and "redaction
  policy non-empty" fail on them unless the risk is accepted. `SecurityLayer::without_csrf()`
  and `RedactionPolicy::new()` are deprecated shims that record the same ids;
  `RedactionPolicy::is_empty()` is unchanged. New `SecurityLayer::csrf_enabled()` says whether
  the layer checks tokens, and `id_effect_kit::posture::Posture::security_layer(&layer)` states
  the "CSRF on" probe from it (`.csrf(bool)` still works).
- The unpublished `tests/security_compile_fail` crate runs 17 trybuild cases, one or more per
  type-enforced ADR 0013 rule (unguarded routes, non-zeroizing secrets, raw SQL without the
  capability, plaintext mesh TCP without the feature, cross-tenant access, accepted risks without
  a reason, …), blessed on the pinned toolchain; the case → rule table is
  `docs/security/compile-fail.md` (E11.4).
- ADR 0013 "Secure by default" (rules R1–R10) and its hook in core:
  `id_effect::observability::posture::record_dangerous(id, reopens)` records an opt-out once per
  process and `dangerous_opt_outs()` lists them; every `dangerous_*` / `insecure_*` API calls it
  and the kit's prod posture check reads it (tests `posture_is_empty_by_default`,
  `posture_records_each_opt_out_once`).
- `id_effect_logger` (M11, M12): with no policy installed the pipeline applies
  `RedactionPolicy::recommended()` (16 field names and the globs `*_token`, `*_secret`,
  `*password*`); `RedactionPolicy::dangerous_none()` turns redaction off (recorded);
  `active_redaction_policy()` is new. `TracingLogBackend` escapes CR, LF, tab and every other
  control character (ANSI, C1, DEL, U+2028/2029) in the message, target, spans, keys and values,
  and quotes values with a space, `=` or `"`; the structured backend escapes keys, so a logged
  value cannot forge a line or a field (tests `m11_default_pipeline_redacts_common_secret_fields`,
  `m12_newline_in_message_cannot_forge_a_line`, `m12_annotation_key_cannot_inject_fields`,
  `m12_control_and_ansi_sequences_are_escaped`). See "Logging: `RedactionPolicy::new`" in the
  [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_config` and `id_effect_auth` secrets (M13, M14, L3): `Secret<T: Zeroize>` zeroizes
  its value on drop; new `SecretString` / `SecretBytes`, the only `Secret`s with `==`, which is
  constant time (`subtle`); `Password` zeroizes its plaintext; `Session`'s `Debug` shows attribute
  keys, never values (tests `m13_secret_is_zeroized_on_drop`, `m13_password_is_zeroized_on_drop`,
  `m14_session_debug_hides_attr_values_and_tokens`, `l3_secret_eq_uses_constant_time_compare`).
  See "Config: `Secret<T: Zeroize>` and its equality" in the
  [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_sql` raw SQL needs a capability (M16, D9): `query`, `execute`, `query_as`,
  `query_one_as`, `query_opt_as`, `query_stream` and `query_stream_as` take `impl Into<Sql>`,
  which only a `&'static str` converts to; SQL built at run time is a `RawSql` run with
  `query_raw` / `execute_raw`, which need a `RawSqlCapability` from
  `RawSqlCapability::layer_dangerous()` (recorded) (trybuild
  `m16_query_with_string_does_not_compile`, `m16_query_raw_requires_capability`,
  `m16_raw_capability_cannot_be_forged`). `id_effect_redis` (M17): `RedisKv` / `RedisHash` /
  `RedisJson::clear()` on the empty namespace fail with `RedisError::EmptyNamespace` instead of
  flushing the database; `clear_all_dangerous()` is the recorded flush
  (`m17_clear_with_empty_namespace_is_an_error`, `m17_clear_only_deletes_namespace_keys`). See
  "SQL: `Sql` and `RawSql`" and "Redis" in the [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_authz` tenant isolation by default (M19): a policy defined for a `Tenanted` resource
  denies a principal of another tenant, or of none, before it runs; resource types implement the
  new `TenantScope`; `PolicyExt::dangerous_cross_tenant(reason)` is the recorded way across
  (tests `m19_tenanted_resource_requires_tenant_match_by_default`,
  `m19_cross_tenant_requires_named_opt_out`, `m19_untenanted_resource_unaffected`). See
  "Authorization: tenant match by default" in the [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_process` bounded mailboxes (M10, D17): 10,000 user messages by default
  (`SpawnOptions::mailbox_capacity`); a full mailbox fails a send with `SendError::Full` and a call
  with `CallError::MailboxFull`, and a remote call with `RemoteFault::MailboxFull`; exit signals
  and monitor downs bypass the bound (tests `m10_bounded_mailbox_rejects_when_full`,
  `m10_system_messages_bypass_bound`, `m10_remote_send_to_full_mailbox_faults`). See "Processes"
  in the [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_auth` per-process login throttle recorded (H5/H6 residual): `MemoryThrottleStore`,
  the default store of `LoginThrottle`, of `BasicAuth::new` and of `Totp::new`'s lockout, records
  `MEMORY_THROTTLE_STORE_POSTURE` (`id_effect_auth::MemoryThrottleStore::memory_only`), so the
  kit probe "no memory-only security state" fails on it in prod. New constructors that never
  create it: `BasicAuth::throttled(users, hasher, throttle)` and
  `Totp::with_stores(config, steps, failures)`; `.with_throttle(..)` / `.with_failure_store(..)`
  after `new` still record. The Ledger counts both in `PgThrottleStore` (its migration
  `0009_auth_failures`) and stays clean in prod (test `h5_memory_throttle_store_is_recorded`).
- New opt-outs, each recorded in the posture registry under its own id and refused by the prod
  posture check unless accepted: `Router::allow_unregistered_routes_dangerous`,
  `Router::route_unchecked` (deprecated), `OpenApiUi::public_dangerous`,
  `HostConfig::dangerous_no_load_shedding`, `SecurityLayer::dangerous_without_csrf`,
  `RedactionPolicy::dangerous_empty`, `RedactionPolicy::dangerous_none`,
  `RawSqlCapability::layer_dangerous`, `RedisKv` / `RedisHash` / `RedisJson::clear_all_dangerous`,
  `PolicyExt::dangerous_cross_tenant`, `SsrfPolicy::dangerous_allow_private_networks`,
  `ReqwestHttpClient::dangerous_unguarded`, `dangerous_provide_reqwest_client`,
  `OidcConfig::insecure_allow_http`, `JwksClient::insecure_allow_http`,
  `BasicAuth::dangerous_without_throttle`, `Totp::dangerous_without_lockout`,
  `JwtVerifier::dangerous_any_issuer` / `dangerous_any_audience`,
  `QuicConfig::dangerous_no_client_auth`, `TlsTcpConfig::dangerous_*`,
  `TcpTransport::dangerous_plaintext` (feature `insecure-plaintext-tcp`); now recorded as well:
  `QuicConfig::dangerous_accept_any_server_cert`, `Cookie::dangerous_allow_short`, the three
  `ConnectionLimits::dangerous_*`, `MemoryTotpStepStore` and `MemoryThrottleStore`.
- Supply chain (E11.3; S1–S3, S5–S7): every workflow action pinned by commit SHA, the toolchain
  pinned (`rust-toolchain.toml`), top-level `permissions: contents: read`, Dependabot no longer
  approves its own pull requests, `Cargo.lock` committed and CI built `--locked`, enforced by
  `scripts/check-workflows.sh`; crates published by trusted publishing only, with SLSA Build L3
  provenance and a CycloneDX SBOM per crate, each uploaded and published crate checked against
  the attested digest, and releases refused without a signed tag (`docs/RELEASING.md`); DCO
  enforced on pull requests (`dco.yml`); CodeQL for Rust and Actions and the OpenSSF Scorecard
  workflow (`docs/security/openssf.md`); `SECURITY.md` for 0.11.x and `THREAT-MODEL.md` (STRIDE
  per trust boundary, findings register: no finding open in code, L6 accepted).
- Fuzzing (E11.4): `fuzz/` with cargo-fuzz targets `schema_json`, `route_path`, `route_query`,
  `route_form`, `cron_parse`, `config_profile_toml`, `jwt_decode`, `jwks_parse`, `totp_otpauth`,
  `mesh_frame` and `mesh_envelope`, run by ClusterFuzzLite on pull requests and nightly
  (`docs/security/fuzzing.md`).

### Performance

- `id_effect_axum` `EffectExecutor` drives each request's root fiber inline, on the task polling
  the handler (or middleware) future, instead of spawning it as a tokio task and awaiting it. An
  effect that completes without suspending costs no spawn and no cross-thread wake-up; the fiber
  goes to the executor only when the caller drops the future early (interrupted there under
  `DropPolicy::Interrupt`, finalizers included, as before). A pinned
  `EffectExecutorBuilder::runtime` still spawns onto that runtime. Every effect bridge uses it
  (routing helpers, `execute*`, `run_with_env`, `Authenticate`, `Guard`), so in the
  `gate_request` bench `axum_request_bare` went from 8.4 µs to 0.90 µs and
  `axum_request_secure_stack` from 28.9 µs to 5.5 µs. Code inside a request effect now runs
  inside the caller's `tracing` span and task, as an `async` handler does.
- `id_effect_auth` `Authenticate` checks an API key from an in-process store without the effect
  runtime: the new `ApiKeyStore::find_now` (default `None`) is a synchronous fast path that
  `MemoryApiKeyStore` implements; a store returning `None` runs `find` as before.
- `id_effect_authz` route guards decide a pure policy without a fiber: the new
  `Policy::decide_now` (default `None`) is a synchronous fast path that the built-in policies
  and combinators implement (`Constant`, `Authenticated`, `RequireRole`, `PredicatePolicy`,
  `And`/`Or`/`Not`, `CrossTenant`, `Arc`/`Box`); a guard with audit events, or
  a policy returning `None`, runs `evaluate` as before.
- `id_effect_axum` `SecurityLayer` renders the CSP header once per policy and fills in only the
  nonce, keys the CSRF cookie HMAC once per layer and masks the token only when it is read;
  `id_effect_logger` reads no fiber ref that is not installed and formats no line `tracing` would
  drop. With the two fast paths above, the secure stack of the `gate_request` bench went from
  5.42 µs to 3.54 µs (bare 0.91 µs: +2.63 µs per request) without changing what any layer checks
  (`docs/benchmarks/README.md`, "Secure-stack budget (0.11)").
- The secure-stack KPI is an absolute budget, `gate/axum_request_secure_stack −
  gate/axum_request_bare ≤ 5 µs`, gated in CI (owner decision 2026-10-19), in place of the ratio
  ≤ 1.15, which a 0.9 µs bare request would turn into about 135 ns for every security layer; the
  ratio is printed for information (see Added, "Bench gates in CI").

### Added

- Optional synchronous fast paths for custom implementors: `id_effect_auth`
  `ApiKeyStore::find_now` and `id_effect_authz` `Policy::decide_now`, both defaulting to `None`
  (see Performance). Existing implementations compile and behave as before; see "Auth and
  authz: optional synchronous fast paths" in the [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_kit` prod posture check (ADR 0013 R10): new `posture` module. `Posture` takes the
  profile, the risks its configuration accepts (`security.accepted_risks`, opt-out id → reason;
  a blank reason does not accept) and what the app states about itself; `check()` adds the
  posture registry and returns a `PostureReport` with nine named probes (console guarded, docs
  closed, mesh transport encrypted, redaction policy non-empty, SSRF guard on, CSRF on, load
  shedding on, no memory-only security state, no unaccepted opt-out). `enforce()` refuses the
  `prod` profile (any but `dev`/`test`) on a failed probe; `dev` and `test` report. New kit
  features `workflow` (`kit::workflow`), `mesh-mtls` (QUIC) and `kubernetes`; `cluster` now
  enables id_effect_node's `tls-tcp`, so `TcpTransport::tls` is reachable through the kit. The
  prelude adds `stm`, `Public`, `Guarded`, `RouteGuard` and `LoadShedLayer` (also re-exported at
  the `id_effect_axum` root). The `service`, `worker` and `cluster` templates run the check at
  startup; the service template declares its plain routes and applies `AccessLayer` after the
  last route (undeclared routes answer 404) and passes the prod check as generated
  (`template_service_passes_prod_posture`); the cluster template builds a mutual-TLS TCP mesh
  transport from `MESH_CA_PEM` / `MESH_CERT_PEM` / `MESH_KEY_PEM`, with throwaway dev
  certificates for `dev`/`test`. New `scripts/check-dangerous.sh` (with `--self-test`, run by the
  lint workflow) fails when a `dangerous_*` / `insecure_*` / `*_dangerous` / `*_insecure`
  function under `crates/` does not call `record_dangerous`. See "Kit and templates: the prod
  posture check" in the [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_node` Kubernetes discovery (dist.membership #4, D18): feature `kubernetes` adds
  `discovery::KubernetesDiscovery`, a `Discovery` strategy that lists the ready addresses of a
  headless service from the in-cluster `discovery.k8s.io/v1` EndpointSlice API over rustls, with
  its own HTTP/1.1 client (no kube-rs). The API server is verified against the service-account CA
  (no opt-out); the bearer token is re-read from the mount on every listing and the namespace
  comes from its file. Not-ready and terminating endpoints are skipped; `endpoints()` returns a
  `KubernetesError`, `candidates()` answers empty so `ChainedDiscovery` can fall through to DNS.
  Book "Finding peers" in "Nodes, membership, and remote processes".
- `id_effect_process` timers and hibernate (fault.actors #6, #8): `ProcessCtx::send_after` and
  `Addr::send_after` return a `TimerRef`; `cancel_timer` / `TimerRef::cancel` returning `true`
  means the message is never delivered. Timers run on the executor clock and die with their
  process. `ProcessCtx::set_idle_timeout` delivers the new `Info::Timeout` once per idle period;
  `Next::Hibernate` runs the new `Process::hibernate` hook, releases the mailbox buffer and resumes
  on the next message. Breaking: `Next`, `Info` and `ExitReason` are `#[non_exhaustive]`, and a
  lost cluster-wide name is `ExitReason::NameConflict(name)` instead of an `Error` string. See
  "Processes" in the [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_fsm` `Statem` (fault.fsm #3, #4, #5): a `gen_statem`-parity behaviour next to the
  unchanged `Fsm` trait. States may carry data (no `Copy`) and a separate `Data` travels with the
  machine; `handle_event` returns `Transition::{Next, Keep, Stop}` with `Action::{Postpone,
  StateTimeout, EventTimeout, GenericTimeout, Reply, NextEvent}`. Postponed events are re-delivered
  after the next state change in arrival order, a state change cancels the state timeout, any event
  cancels the event timeout, and the optional `on_enter` callback runs on every state change. It
  runs as a supervised process (`StatemProcess::start` / `start_linked`, `StatemHandle::call`).
  Book "`Statem`: gen_statem parity", example `statem_door_lock`, test
  `crates/id_effect_fsm/tests/statem.rs`.
- `id_effect::stm` collections (conc.stm #6): `TSet`, `TArray` (one `TRef` per element),
  `TPriorityQueue` (`take` returns the smallest element and parks while empty), `TPromise`
  (completes once; `await_` parks until completed) and `TReentrantLock` (read/write lock,
  reentrant per fiber: a transaction knows the `FiberId` running `commit`). All compose with
  `Stm::or_else` and `Stm::retry`. Book "Transactional Collections" covers all nine STM
  structures; examples `087`–`091_stm_*`; test `crates/id_effect/tests/stm_collections.rs`.
- Bench gates in CI (test.qa #8): `.github/workflows/bench.yml` benches base and head on one
  runner on pushes to `main` and on pull requests labelled `perf`, and fails on a `budget` or
  `delta` gate. `scripts/bench-compare.sh` learns `kind = "pair"` (an intra-run ratio),
  `kind = "delta"` (an intra-run difference in ns) and `--enforce KINDS`; `gates.toml` gains the
  secure-stack KPI gate `gate/axum_request_secure_stack − gate/axum_request_bare ≤ 5000 ns`
  (`id_effect_axum` bench `gate_request`): an absolute budget of 5 µs per request (owner
  decision 2026-10-19) instead of the ratio ≤ 1.15, which is now printed for information only.
  See "CI job" and "Secure-stack budget" in `docs/benchmarks/README.md`.
- `id_effect_node` at-least-once delivery (D19) and remote trace context (dist.remote#8):
  at-most-once stays the default; opt in per send (`Mesh::send_reliable`, `Mesh::cast_reliable`,
  `SendReliable::send_reliable` on an `Addr`) or per mesh (`Mesh::with_delivery(DeliveryConfig::
  at_least_once())`). Per-peer sequence numbers, `ProcessEnvelope::Ack`, a retransmit buffer
  bounded in messages and bytes (the oldest is dropped when full) with exponential backoff, and a
  receiver dedup window keyed by (incarnation, seq), so a reconnect does not deliver twice;
  messages pending when the peer restarts, or dropped from a full buffer, are reported as
  `DeliveryFailed` to the `Mesh::monitor_delivery` monitors. `Mesh::cast`, `Mesh::send` and
  `Mesh::remote_call` carry the sending fiber's span (`TracePropagator`, default
  `W3cTraceContext`), and the receiver opens `mesh.call` / `mesh.cast` / `mesh.info` under it.
  Breaking: the mesh protocol is version 3; 0.10.x nodes (versions 1 and 2) are refused with
  `HandshakeError::VersionMismatch`. Fuzz targets `mesh_frame`, `mesh_envelope`. See "Node: mesh
  protocol version" in the [0.11 migration guide](docs/MIGRATION-0.11.md).
- `id_effect_workflow`, published (tier integration; fault.workflows #1 #2 #3 #4 #5 #7, level
  documented): durable workflows journaled in Postgres on `id_effect_sql`, replayed on resume, with
  any number of engines per database under fenced leases; activities and durable timers as
  `id_effect_jobs` jobs; signals; and durable sagas: `WorkflowContext::saga` registers
  compensations as journaled activities that run in reverse order, at least once, when the body
  fails, and survive restarts (`tests/saga_pg.rs`). Book "Workflow and cluster" rewritten,
  examples `010_order_saga`, `020_durable_timer`, `030_signal_approval`. Removed: the unpublished
  `labs/id_effect_workflow` with its duroxide, duroxide-pg and rusqlite dependencies, the
  `NetworkJournalStub` and the FSM bridge. See "Workflows" in the
  [0.11 migration guide](docs/MIGRATION-0.11.md).
- The Ledger reference app adopts 0.11 (the Ledger KPI): `main.rs` runs the prod posture check
  over the assembled app and the `prod` profile passes it with no accepted risk
  (`ledger_prod_posture_is_clean`); every route, typed or plain, is declared public or guarded and
  a route without a declaration answers 404 (`ledger_access_report_snapshot`,
  `h8_ledger_unregistered_route_denied`); the console and `/docs` are off in prod
  (`m6_ledger_console_disabled_in_prod_profile`); the SendGrid host is the SSRF allowlist of prod.
  Account closure runs as a durable workflow (`POST /accounts/{id}/closure`): an owner's approval
  signal, a 30-day grace timer, the final statement exported by an activity on the job queue, and
  a saga compensation that unlocks the account on a rejection
  (`account_closure_workflow_survives_restart`). This makes the Ledger the reference-app consumer
  of `id_effect_workflow`, now tier `platform` (ADR 0012, check P). The `reference-ledger` CI job
  runs these tests first.
- `id_effect_node` cluster-wide names (dist.placement #5): `Mesh::global()` returns a
  `GlobalRegistry` (`register`, `whereis`, `unregister`, `cast` / `call` by name, `sync_with`);
  every node holds the table, only the owner registers its process, names go with the process or
  its node, and conflicting claims after a partition heal to one owner, the loser exiting with
  `ExitReason::NameConflict` (`tests/global_registry.rs`, example `135_global_names`, book
  "Cluster-wide names").
- `id_effect_process` selective receive (E8.1): `ProcessCtx::receive(matcher, timeout)` takes the
  first matching message and leaves the skipped ones queued in order, bounded by the mailbox
  capacity (`ReceiveError::{Timeout, ScanLimit, Closed}`); `SpawnOptions`, `Mailbox::with_capacity`,
  `try_offer_now` and `OfferError` come with the bounded mailboxes (see Security).
- `id_effect_lints` (E1.9): the labs lint crate is a Dylint library at `lints/id_effect_lints`,
  run with `cargo dylint` in a CI job, every lint at warn, with the new `raw_sql_non_literal`
  (ADR 0013 R5) and the auto-fixable `effect_map_identity`, `effect_flat_map_succeed` and
  `effect_flat_map_then`. Its ast-grep rules load again (one rule per YAML document, plus
  `utils/`; same ids and severities) and run in a new `ast-grep` job of the lint workflow;
  `lints/id_effect_lints/examples/demo` is a crate both fire on. Book appendix D. See "Lints" in
  the [0.11 migration guide](docs/MIGRATION-0.11.md).
- Book: "Trace context across nodes" in chapter 38 (the `TracePropagator` of the effect sends, the
  receiving `mesh.*` spans, the OpenTelemetry recipe), "Transport security: mutual TLS",
  "Cluster-wide names", "Every route declares its access", "Tenant match in policies",
  "Transactional Collections", "`Statem`: gen_statem parity" and the rewritten "Workflow and
  cluster".

### Changed

Every break below is in the [0.11 migration guide](docs/MIGRATION-0.11.md), with its shim or the
reason there is none; its last section maps each `cargo semver-checks` report against 0.10.1 to
its entry.

- **Breaking** `id_effect_axum`: `Router::route` takes only an endpoint whose route is `.public()`
  or `.guard(g)` (`Route` and `Endpoint` gain an access parameter); `RouteInfo` gains `access`;
  `into_axum()` answers 404 to unregistered routes; `HostConfig` gains `load_shed`; the CSRF token
  is masked (86 characters, new on every response); `OpenApiUi` mounts nothing outside `dev` /
  `test` unless guarded, no longer derives `PartialEq` and is no longer `UnwindSafe`.
- **Breaking** `id_effect_console`: `ConsoleConfig` is `Clone`, no longer `Copy`.
- **Breaking** `id_effect_config`: `Secret<T>` requires `T: Zeroize`; `==` only on `SecretString` /
  `SecretBytes`.
- **Breaking** `id_effect_sql`: the query free functions take `impl Into<Sql>`, not
  `impl Into<String>`.
- **Breaking** `id_effect_redis`: `clear()` on the empty namespace fails with
  `RedisError::EmptyNamespace`.
- **Breaking** `id_effect_authz`: tenanted resources check the tenant first; resource types need
  `TenantScope`.
- **Breaking** `id_effect_logger`: no installed policy means `RedactionPolicy::recommended()`; line
  formats escape control characters and quote values.
- **Breaking** `id_effect_platform`: the HTTP clients refuse private destinations and follow no
  redirect; `HttpClientConfig` gains `ssrf` and `max_redirects`.
- **Breaking** `id_effect_auth`: OIDC requires `https` endpoints outside loopback.
- **Breaking** `id_effect_node`: QUIC listeners require client certificates; `TcpTransport` is
  TLS by default, no longer a unit struct or `Copy`, plaintext only with `insecure-plaintext-tcp`;
  mesh protocol version 3 refuses 0.10.x peers; `ProcessEnvelope` gains `GlobalRegister`,
  `GlobalUnregister`, `GlobalSync`, `Reliable` and `Ack`, `RemoteFault` gains `MailboxFull` and
  `ExitReasonWire` gains `NameConflict`.
- **Breaking** `id_effect_process`: `Next`, `Info`, `ExitReason`, `SendError` and `CallError` are
  `#[non_exhaustive]`, with the new `Next::Hibernate`, `Info::Timeout`,
  `ExitReason::NameConflict`, `SendError::Full` and `CallError::MailboxFull`; mailboxes are
  bounded.
- `id_effect`: STM is the core module `id_effect::stm` (D5), always compiled, same semantics.
- `id_effect_axum`: request effects run inline on the caller's task, inside its `tracing` span
  (see Performance).

### Deprecated

One-minor shims (L9), removed in 0.12; each records the same posture id as its replacement:

- `id_effect_axum`: `Router::route_unchecked` (for `.public()` / `.guard(g)`),
  `SecurityLayer::without_csrf` (for `dangerous_without_csrf`).
- `id_effect_logger`: `RedactionPolicy::new` (for `recommended()` or `dangerous_empty()`).
- `id_effect_auth`: `BasicAuth::without_throttle_dangerous`, `Totp::without_lockout_dangerous`,
  `JwtVerifier::any_issuer_dangerous` / `any_audience_dangerous`, `JwksClient::allow_http_dangerous`
  (for `dangerous_without_throttle`, `dangerous_without_lockout`, `dangerous_any_issuer` /
  `dangerous_any_audience`, `insecure_allow_http`).
- `id_effect_platform`: `provide_reqwest_client` and `ReqwestHttpClient::new` (for
  `dangerous_provide_reqwest_client` / `dangerous_unguarded`, or the guarded `layer_with` /
  `from_config`).
- `id_effect_fp`: `stm::*` and the root STM re-exports, now aliases of `id_effect::stm`.

### Removed

- **Breaking** The 0.9 deprecations, as announced in 0.10 (L9): `id_effect_sql::SqlParam` (with
  `From<SqlParam> for SqlValue` and `SqlValue == SqlParam`; use `SqlValue`, `Int` is `I64`),
  `SqlRow::new` (use `SqlRow::from_pairs`), `SqlRow::text_cells` (use `row.get::<T>(idx)`);
  `id_effect_axum::json::decode_json_schema`, `JsonSchemaError`, `SchemaJsonErrorBody` (use
  `json::decode_json` or `route::ValidJson`, which answer an RFC 9457 `Problem`). See "Removed
  (promised in 0.10)" in the [0.11 migration guide](docs/MIGRATION-0.11.md).
- `labs/id_effect_workflow` (with duroxide, duroxide-pg, rusqlite, the `NetworkJournalStub`, the
  journal transport and the FSM bridge), replaced by the published `id_effect_workflow`.
- `labs/id_effect_lint`, replaced by `lints/id_effect_lints`.
- `id_effect_fp`'s own STM implementation (its paths remain as deprecated aliases).
- The unreleased `RedactionPolicy::empty()` of early 0.11 development.

### Scorecard

Measured by blind re-score (`docs/scorecard/REPORT-0.11.0.md`, METHOD 2.2). Method 2.2 itself
landed at the 0.11 kickoff: non-goal rows and checks leave the score, .NET's scope adds Orleans and
the Durable Task framework, and 0.6.0–0.10.0 were re-scored (`docs/scorecard/REPORT-method-v2.md`),
so the 0.10.0 column below is the method 2.2 re-score:

| Index | 0.10.0 | 0.11.0 | Target |
|---|---|---|---|
| Universal (U) | 76.6 (#2) | 79.4 (#1) | — |
| Enterprise (E) | 76.6 (#2) | 79.2 (#1) | 79.2 (#1) |
| Runtime (RT) | 79.5 (#1) | 83.9 (#1) | 83.7 (#1) |
| Rows < 40 | 11 | 9 | ≤ 9 |
| Rows ≥ 80 | 49 | 56 | 56 |
| Rows where id_effect leads | 26 | 31 | — |

11 rows up, none down; id_effect passes .NET (78.8) on E. Distribution 68.8 → 88.2, Fault
tolerance 78.9 → 91.6; biggest rows `dist.tls` 50 → 89, `fault.workflows` and `dx.lint` 38 → 75,
`fault.actors` and `fault.fsm` 63 → 89. Every 0.11-targeted row met its target except
`sec.supply` (88, target 100): `sec.supply#7` needs a published GitHub Security Advisory and is
carried over to 0.12.0. DX KPIs (`docs/benchmarks/dx-kpis-0.11.json`): K1 23.5 s, K2 1.32 s, K3
634 lines, K4 17/20, K5 91.7 %, K6 36.4 s. Security KPIs (`docs/security/kpis-0.11.json`): 0 open
highs, ADR 0013 10/10 rules with an enforcement point, secure stack +2696 ns over bare (budget
5000 ns).

## 0.10.1 — 2026-10-18 — Security patch

Closes the high-severity findings of the 2026-10-18 security review (all but H8, deny-by-default
routing, which changes the API and lands in 0.11) and six medium and low ones. The public API is
compatible with 0.10.0: every change is additive or a stricter default. Each stricter default has
an explicit opt-out whose name contains `dangerous` and whose docs say what it re-opens. Upgrading:
see "0.10.1: stricter security defaults" in the [0.10 migration guide](docs/MIGRATION-0.10.md).

**Upgrade every node of a mesh cluster together**: the mesh handshake protocol moves to version 2,
and 0.10.0 and 0.10.1 nodes refuse each other with `VersionMismatch`.

### Security

- `id_effect_axum` (H1): the serve functions dropped connection info, so per-IP rate limits saw no
  peer and every client shared one bucket. Every serve path now runs on `serve_listener`, which
  records `ConnectInfo<SocketAddr>`. New `ClientIp` extractor and `TrustedProxies`:
  `X-Forwarded-For` is read only when the socket peer is a trusted proxy, from the right, skipping
  trusted hops; with no trusted proxies (the default) it is never read.
  `RateLimitKey::client_ip(TrustedProxies)` keys limits on it.
- `id_effect_axum` (M1): `serve_listener` applies a header-read timeout (default 10 s, also for
  idle and silent connections), a connection cap (default 10,000) and a keep-alive limit (1,000
  requests per connection), set with `ConnectionLimits` and the new `*_with_limits` serve
  variants. Opt-outs: `dangerous_no_header_read_timeout`, `dangerous_unlimited_connections`,
  `dangerous_unlimited_requests_per_connection`.
- Ledger reference app (H2, H5): its login limiter keys on `ClientIp` (config
  `http.trusted_proxies`, empty by default) instead of trusting the first `X-Forwarded-For`
  entry, and its password login runs `LoginThrottle` before hashing.
- `id_effect_node` (H3): QUIC no longer accepts any server certificate by default. Dialing needs
  trusted roots, a pinned leaf-certificate SHA-256 (`QuicConfig::with_pinned_server_cert_sha256`)
  or the explicit `QuicConfig::dangerous_accept_any_server_cert()`. `QuicTransport::try_new`
  reports a missing trust model as `QuicConfigError::NoServerTrust`; `QuicTransport::new` without
  one fails at dial time. TCP logs a one-time warning that it is plaintext.
- `id_effect_node` (H4): the handshake proof covers the whole exchange (version, role, both node
  names and incarnations, both nonces) and, on QUIC, a TLS exporter secret, so a relayed, reflected
  or replayed proof fails. Handshake protocol version 2.
- `id_effect_node` (M9): handshakes time out after 5 s (`Cookie::with_handshake_timeout`), frames
  are capped at 4 KiB until authentication, cookies shorter than 32 bytes are refused
  (`Cookie::try_new`, `CookieError::TooShort`; opt-out `Cookie::dangerous_allow_short`), and nonces
  are 128-bit.
- `id_effect_auth` (H5): `LoginThrottle` (new `throttle` module) limits failed logins per account
  and per client IP: 5 free failures, then a doubling delay up to 15 minutes. It is on by default in
  `BasicAuth` and runs before the user lookup and before hashing; a throttled attempt is
  `AuthError::TooManyAttempts`, 429 with `Retry-After`. Opt-out:
  `BasicAuth::without_throttle_dangerous`.
- `id_effect_auth` (H6): TOTP verification locks after 5 wrong codes (`TotpError::Locked`); a valid
  code resets the count. Opt-out: `without_lockout_dangerous`.
- `id_effect_auth` (H7): a `JwtVerifier` with neither an issuer nor an audience fails every token
  with `AuthError::Config`. Opt-out: `any_issuer_dangerous()` plus `any_audience_dangerous()`.
- `id_effect_auth` (M15): JWKS documents are capped at 1 MiB, fetched over https only (plain http
  only for loopback hosts; opt-out `allow_http_dangerous`) and without following redirects. HMAC
  secrets shorter than 32 bytes fail with `AuthError::Key`.
- `id_effect_auth` (M20): password hashing and verification share a bounded pool (default 2 ×
  available parallelism); a call that waits more than 5 s fails with `PasswordError::Busy`, which
  the account flows answer with 503.
- `id_effect_auth` (L4): `BasicAuth` rejects passwords over 256 bytes before hashing.
- `id_effect_platform` (M2): the HTTP client rejects a declared `Content-Length` over
  `max_body_bytes` before reading the body and stops reading a streamed body as soon as it passes
  the cap.

### Fixed

- `id_effect_mail`: the SMTP connection-reuse test no longer races lettre's pool (it failed the
  first v0.10.0 publish run).
- `id_effect_auth`: `api_keys_authenticate_and_bad_keys_are_401` no longer fails 1 run in 16.

## 0.10.0 — 2026-10-18 — Async work & operations (breaking)

The third release under the plan of plans: a PostgreSQL job queue with cron, a broker-agnostic
messaging API with a transactional outbox and Redis Streams, mail with templates and delivery
through the queue, object storage, a published event-sourcing crate, and an operator console;
plus HTTP client defaults, tracing of outbound calls and queries, log redaction, config hot
reload, fault injection, `id-effect dev` and `id-effect generate job`. The Ledger reference app
uses all of it in CI. Upgrading from 0.9: see the
[0.10 migration guide](docs/MIGRATION-0.10.md).

### Added

- New crates (ADR 0010 Amendment 6): `id_effect_jobs`, `id_effect_messaging`, `id_effect_mail`,
  `id_effect_console`, `id_effect_storage` (tier `platform`; the Ledger is their CI-tested
  consumer) and `id_effect_events`, promoted from `labs/` (tier `integration`).
- `id_effect_jobs`: `Job` trait (payload `Schema`, `NAME`, `QUEUE`, priority, max attempts,
  unique key, timeout, retry `Schedule`), `JobRegistry`, `JobQueue` (`enqueue`, `cancel`,
  `retry_now`, `stats`, `list`) and `JobStore` with `PgJobQueue` and `MemoryJobQueue`.
  `enqueue_in(&tx, …)` enqueues inside the caller's transaction; a unique-key duplicate never
  aborts it. One `id_effect_jobs` table, shipped as `migrator()` or `ensure_schema()`.
- `id_effect_jobs`: `Worker` claiming with `FOR UPDATE SKIP LOCKED` by priority then `run_at`,
  per-queue concurrency, lease heartbeats and rescue of expired leases, retries replayed from the
  job's `Schedule`, dead jobs after the last attempt, graceful drain, `JobObserver` hooks; "now"
  comes from the app's clock. Book part6/ch31-01-jobs.md; example `010_jobs`.
- `id_effect_jobs` feature `cron`: `enqueue_on_tick` (one job per tick, deduplicated by
  `tick_key` across schedulers) and `CronLeader`, which runs the schedule on one node through a
  `ClusterSingleton` over `PgAdvisoryLease` (re-exported with `NodeName` / `Pid`). Core
  `id_effect`: additive `CronJob::new_at`, whose factory receives the fire time.
- `id_effect_messaging`: object-safe `MessageBroker` (`publish`, `subscribe` per
  `ConsumerGroup` with prefetch, concurrency, max deliveries, dead-letter topic and start
  position); `Delivery` settled with `ack` / `nack(Requeue | DeadLetter)`; `Message` with id, key,
  headers and a `Schema`-typed payload; `MessageBrokerExt::consume`, `ack_on_success`;
  `MemoryBroker` with test helpers. Book part6/ch31-02-messaging.md.
- `id_effect_messaging`: transactional `Outbox::stage(&tx, …)`, a `Relay` to any broker
  (`SKIP LOCKED` claims under a lease, retries on a `Schedule`, dead rows, pruning; at-least-once)
  and an idempotent `Inbox` deduplicating on (consumer group, message id) in the handler's
  transaction; `PgOutbox`, `MemoryOutbox`, `MemoryInbox`, `migrator()`. Example `030_outbox_inbox`.
- `id_effect_redis` feature `streams`: `RedisStreamsBroker`, a `MessageBroker` on Redis Streams
  (≥ 6.2): consumer groups, `XAUTOCLAIM` redelivery, prefetch credit, requeue, dead-lettering
  that cannot run twice. Example `030_streams`; book ch28-04.
- `id_effect_mail`: `Message` / `MessageBuilder` (attachments, inline `cid:` parts, header
  injection refused), the `Mailer` capability, `SmtpMailer` (lettre, rustls), `SendGridMailer`
  (POST never retried) and `TestMailbox` with assertions; `MailError::is_transient`.
- `id_effect_mail`: minijinja templates (`{mail}.html` / `.txt` / `.subject.txt`, layouts and
  partials, HTML autoescaped); feature `preview` serves `/_mail/previews`; feature `jobs` adds
  `deliver_later` / `deliver_later_in(&tx, …)` through `id_effect_jobs` (12 attempts over ~5 h,
  permanent failures go dead at once, payloads capped at 2 MiB). Book part6/ch31-03-mail.md.
- `id_effect_console`: guarded operator console. `Console::new()….router(guard)` is the only way
  to get its routes; JSON API under `/_console/api` for fibers, supervisors, metrics, health and
  the runtime log level (GET / PUT); a static dashboard page at `/_console` under a strict CSP;
  feature `jobs` adds a jobs panel with retry / cancel actions. Book part6/ch27-03-console.md.
- `id_effect_storage`: `ObjectStore` capability (`put`, `put_stream`, `get`, `get_stream`,
  `head`, `exists`, `delete`, `list`, `signed_url`) with `MemoryStore`, `LocalStore` and
  `S3Store` (feature `s3`); streaming uploads hold at most one part; HMAC `UrlSigner` for the
  local backends; re-exports `bytes::Bytes`. Book part6/ch28-05-storage.md.
- `id_effect_events` (published): `EventStore::append_expected` with optimistic concurrency,
  `SqlEventJournal` over any `SqlClient` (PostgreSQL and SQLite), CQRS dispatch and projections
  as `Effect`s, snapshots (`SnapshotStore`, `load_aggregate`), a gap-free global `position`,
  `EventLog::read_all` and checkpointed `CatchUpSubscription`s. Book part5/ch23.
- `id_effect_auth` feature `accounts`: registration, email verification and password reset
  (`Accounts`, `AccountRoutes`, `PgAccountTokenStore` + migration
  `0002_id_effect_account_tokens`); `id_effect_mail` feature `auth`: `AccountMails`. Book
  part6/ch30-03.
- Core `id_effect` feature `fiber-registry` (off by default): `fiber_dump()` lists every live
  fiber with parent, children, state, age and span chain. `Schema::with_decode_unknown_all` is
  public.
- `id_effect_logger`: `RedactionPolicy` (`provide_redaction_policy`), `SamplingBackend` /
  `SamplingStats`, `LogRecord::target` with `EffectLogger::log_target`, and a process-wide
  minimum level (`set_global_minimum_log_level`).
- `id_effect_opentelemetry`: configurable sampling (`OtelConfig::sampler`, `OtelSampler`,
  `OTEL_TRACES_SAMPLER[_ARG]`); `TracedHttpClient` (one client span per call, `traceparent`
  injected; `layer(inner)`); feature `sql` with `instrumented_sql_client` /
  `instrumented_sql_layer`; a reloadable `EnvFilter` (`EnvFilterHandle`); `otel.kind` sets the
  span kind. Book ch27-00.
- `id_effect_platform`: `HttpClientConfig`, `RetryPolicy` / `RetryingHttpClient`,
  `ReqwestHttpClientProvider::layer_with`, `provide_reqwest_http_client_with`,
  `ReqwestHttpClient::with_timeouts`. Example `040_http_client_retry`.
- `id_effect_sql`: `SqlError::ConnectionFailed` / `TlsFailed`, `SqlError::sqlstate()`,
  `is_unique_violation()`, `SQLSTATE_UNIQUE_VIOLATION`; `InstrumentedSqlClient` emits a
  `db.query` span per statement (no bound parameters); `SqlClient` for `Arc<T>` and `Box<T>`;
  `PgPoolConfig::connect_options` is public; `TestSqlClient::delay_queries`.
- `id_effect_config`: `Profiles::watch` / `watch_for` / `watch_with` return a `ProfileWatch`
  that hot-reloads the profile files and publishes each good value through a `SubscriptionRef`.
  Book ch16-03; example `007_watch`.
- `id_effect_axum`: `track_in_flight(router, &host_drain)` counts requests for the drain;
  `SecurityLayer::csrf_exempt_bearer()`.
- `id_effect_test`: feature `fault` (`Fault`, seeded `FaultPlan` on the virtual clock; book
  ch15-09, example `030_faults`); Mailpit and S3-compatible (SeaweedFS) containers.
- `id_effect_tool`: `id-effect dev` (watch, rebuild, restart on success; a failed build keeps the
  running app; `--services` starts compose services). Book ch34-02.
- `id_effect_tool`: `id-effect generate job <name> [--queue] [--cron]` adds a job module, its
  registration, a test and the `id_effect_jobs` dependency to a generated project; never
  overwrites.
- `id_effect_tool`: `id-effect new --template service` gains `--jobs` (a job registry, queue and
  worker; Postgres-backed with `--db pg`) and `--console` (the operator console on a loopback
  port); every template ships a `.devcontainer/`; the service's Kamal config checks `/ready` and
  out-waits the drain. Book ch34 "Rolling restarts and drain".
- `id_effect_kit`: features `jobs`, `jobs-cron`, `messaging`, `redis-streams`, `mail`,
  `console`, `storage`, `storage-s3`, `http-client`, `logger`, `events` and `auth-accounts`, all
  in `full`; `test` includes `id_effect_test/fault`; `sql` turns on the OTel SQL spans when
  `otel` is on. Tier `platform`.
- `examples/reference/ledger`: monthly statements (cron leader → one job per account → CSV in S3
  → mail with an attachment or signed link), postings through outbox → Redis Streams → inbox,
  registration / verification / password reset, `/_console` behind an operator role with TOTP,
  CSRF protection, traced HTTP and DB calls, hot-reloaded login limits and a seeded fault plan
  on the posting path.

### Changed

- **Breaking** `id_effect_logger`: `LogRecord` is `#[non_exhaustive]`; build it with
  `LogRecord::new` and the `with_*` methods.
- **Breaking** `id_effect_opentelemetry`: `OtelConfig` gains the `sampler` field.
- **Breaking** `id_effect_sql`: network and TLS failures are `SqlError::ConnectionFailed` /
  `TlsFailed` instead of `QueryFailed`; PostgreSQL errors carry `(SQLSTATE …)`; MySQL duplicate
  keys and SQLite unique violations report `23505`.
- **Breaking** `id_effect_sql`: the backends no longer enable sqlx's `time` feature (reverting
  0.9); MySQL date-time parameters bind as text literals.
- **Breaking** `id_effect_sql`: `SqlClient` is implemented for `Box<T>` and `Arc<T>`; an
  `impl SqlClient for Box<YourClient>` of your own now conflicts with it.
- **Breaking** `id_effect_events`: the published API differs from the labs crate (effects,
  `append_expected`, `SqlEventJournal` over `SqlClient`, the journal's `position` column).
- **Breaking** `id_effect_tool`: `cli::NewArgs` gains `jobs` and `console`.
- `id_effect_platform`: the reqwest layer has a 10 s connect / 30 s total timeout and retries
  idempotent requests (`GET` / `PUT` / `DELETE`) on connect errors, `5xx` and `429`, up to 3
  attempts, honouring `Retry-After`.
- `id_effect_logger`: `provide_composite_logger` and `provide_redaction_policy` are process-wide
  (they were per thread); minimum level, annotations and spans stay per thread.
- `id_effect_sql`: `#[from_row(schema)]` on a derived type lists every invalid column in
  `SqlError::InvalidRow`; derived schemas report every invalid field from `decode_unknown_all`.
- `id_effect_sql`: PostgreSQL date-times use the crate's own binary codecs; values jiff cannot
  hold (`±infinity`, years beyond ±9999) are `SqlError::ValueOutOfRange`, never a panic.
- `id_effect_node`: `PgAdvisoryLease::from_pool_config` applies the pool config's TLS settings
  (it used only the URL); QUIC uses rustls on aws-lc-rs.
- `id_effect_tool` `service` template: `/ready` turns 503 on SIGTERM and in-flight requests
  drain; request spans come from `RequestIdLayer::span_hook` instead of a root-fiber hook.

### Deprecated

- Nothing new. The 0.9 deprecations (`id_effect_sql::SqlParam`, `SqlRow::new`,
  `SqlRow::text_cells`; `id_effect_axum::json::decode_json_schema`, `JsonSchemaError`,
  `SchemaJsonErrorBody`) stay one more release to keep 0.10's breaking changes to the new
  batteries; they are removed in 0.11 (MIGRATION-0.9 and the book now say so).

### Removed

- `labs/id_effect_jobs` (with its apalis, obix, `job` and rdkafka dependencies) and
  `labs/id_effect_graph`; see [MIGRATION-0.10.md](docs/MIGRATION-0.10.md) for the replacements.
- `id_effect_events`: the `es-entity` feature (`EsEntityEventStore`, `EsEntityPgBackend`, its DDL
  helpers and providers, `dispatch_command_es_entity`), `SqlJournalBackend` /
  `TestSqlJournalBackend`, and the `id_effect_graph`, es-entity, sqlx and tokio dependencies.
- `id_effect_sql`: the `time` dependency and sea-query's `with-time`.

### Fixed

- `id_effect_node`: `ClusterSingleton::resign` clears the local pid after a lost lease;
  `PgAdvisoryLease::try_acquire` no longer "re-acquires" after its lock session died.
- `id_effect_proc_macro`: `#[derive(Schema)]` is hygienic (a field named `obj` or `__issues` no
  longer shadows the decoder's locals) and reports an unsupported field type once, not three times.
- `id_effect_tool` `service` template: `/ready` did not see the server's readiness, so it never
  turned 503 on SIGTERM, and nothing was drained.
- `id_effect_sql`: `InstrumentedSqlClient` forwards `begin_with` and `query_stream` (wrapping a
  `PgSqlClient` lost server-side streaming and custom transaction options).
- `id_effect_node`: quinn's default features pulled in rustls' `ring` provider next to aws-lc-rs,
  so TLS configs built from the default panicked when `quic` was enabled with sqlx TLS.
- `id_effect_messaging`: `Inbox::handler` / `MemoryInbox::handler` no longer borrow the inbox.
- `id_effect_axum`: `trace_request` no longer holds its span entered across `.await`.
- `id_effect_console`: the dashboard sends the CSRF token, so its log-level and job actions work
  behind `SecurityLayer::recommended`.
- `id_effect_test`: a container start that loses a port-publish race ("address already in use",
  "port is already allocated") is retried with fresh ports instead of failing the test.

### Security

- `id_effect_logger`: the redaction policy and composite backend applied only on the thread that
  installed them, so lines logged on server worker threads reached `tracing` unredacted; both
  are process-wide now.
- `id_effect_config`: config parse errors no longer quote the offending source line or values
  (a malformed line holding a secret reached `ConfigError`, logs and
  `ProfileWatch::last_error()`).
- `id_effect_auth` accounts: verification and reset tokens are stored only as SHA-256 digests,
  compared in constant time and single-use; `forgot_password` answers identically for unknown
  addresses; a reset revokes the account's sessions. Account mails bypass the job queue, so a
  usable token never sits in the jobs table.
- `id_effect_console`: every route sits behind the caller's guard (including the 404 fallback);
  job actions require `application/json`, so a cross-site form cannot trigger them; the page
  sends `SecurityLayer`'s CSRF token.
- `id_effect_axum`: `SecurityLayer::csrf_exempt_bearer()` skips CSRF only for requests with
  `Authorization: Bearer` and no `Cookie` header; cookie, Basic and other schemes are still
  checked.
- `id_effect_mail`: SMTP / SendGrid credentials print as `<redacted>` in `Debug`.
- `deny.toml`: 0BSD allowed (quoted_printable, via lettre).

### Scorecard

Measured by blind re-score (`docs/scorecard/REPORT-0.10.0.md`, METHOD 2.1):

| Index | 0.9.0 | 0.10.0 | Target |
|---|---|---|---|
| Enterprise (E) | 67.2 (#3) | 75.5 (#2) | 73.8 (#2) |
| Runtime (RT) | 70.8 (#2) | 78.4 (#1) | 76.8 (#1) |
| Rows < 40 | 21 | 12 | ≤ 12 |
| Rows ≥ 80 | 38 | 47 | 44 |
| Rows where id_effect leads | 19 | 24 | — |

17 rows up, none down. Messaging & jobs 45.4 → 87.6, Observability 70.4 → 92.6; biggest rows
`msg.jobs` 13 → 100, `msg.email` 1 → 75, `obs.console` / `msg.brokers` / `dx.repl` 25 → 75,
`data.events` 39 → 88. Every 0.10-targeted row met or beat its target; `dx.lint` moved to 0.11
by the D1 checkpoint. DX KPIs (`docs/benchmarks/dx-kpis-0.10.json`): K1 19.90 s, K2 0.97 s, K3
632 lines including a background job, K4 17/20, K5 93.3 %, K6 22.52 s. Bench gates: no budget
gate fails against 0.9.0, with `fiber-registry` off or on.

## 0.9.0 — 2026-10-17 — Data & security (breaking)

The second release under the plan of plans: typed SQL with migrations, repositories, three
databases and TLS; typed routes with OpenAPI; authentication and authorization; test containers;
and the Ledger reference app that uses all of it in CI. Upgrading from 0.8: see the
[0.9 migration guide](docs/MIGRATION-0.9.md).

### Added

- New crates (ADR 0010 Amendment 5, tier `platform` since the Ledger reference app uses them):
  `id_effect_auth`, `id_effect_authz`, `id_effect_test`, `id_effect_redis`.
- `examples/reference/ledger`: the Ledger reference app (CI job `reference-ledger`). Double entry
  over typed routes, `SERIALIZABLE` postings, PostgreSQL sessions, JWT, OIDC + PKCE, TOTP, RBAC, a
  Redis cache, a read replica and Vault, built only through `id_effect_kit`. New on the way:
  `Route::handler_with_request_caps` (`id_effect_axum`), kit feature `audit`.
- `id_effect_redis`: pooled `RedisClient` layer; typed `RedisKv` / `RedisHash` / `RedisJson`
  stores; `RedisCacheBackend`; pub/sub as a `Stream`; `RedisSessionStore` (feature `session`);
  `RedisRateLimitStore` (feature `rate-limit`). Core `id_effect`: additive `CacheBackend` trait and
  `Cache::with_backend` (same `Cache` API, in-process single-flight kept, backend failures counted
  by `Cache::backend_errors`).
- `id_effect_config`: per-environment profiles (`Profiles`, `ID_EFFECT_PROFILE` / `--profile`)
  layering `config/base.toml` → `config/<profile>.toml` → `APP_*` env (env > profile > base); an
  unknown profile is `ConfigError::UnknownProfile`; `Profiles::overlay` takes a secret provider.
- `id_effect_axum::route`: typed routes —
  `Route::get("/accounts/{id}").path::<P>().query::<Q>().headers::<H>().body::<B>().response::<R>(status).handler(f)`
  with `HasSchema` parts, every decode issue reported at once (422); `route::Router` keeps a
  `RouteRegistry` of `RouteInfo` (method, path, schema per part, JSON Schema 2020-12) for OpenAPI
  and route listings. The existing `routing::get/post/…` are unchanged.
- `id_effect_axum`: RFC 9457 `Problem` (`application/problem+json`) with `errors[]` of
  `{in, pointer, message}`; typed routes answer invalid input with it (422, or 400 for malformed
  JSON) and list every issue. Standalone extractors `ValidPath`, `ValidQuery`, `ValidHeaders`,
  `ValidJson`, `ValidForm` (urlencoded and multipart text fields) share the same decoder.
  `Route::form::<T>()`, `RouteInfo::body_kind`, `json::decode_json`. Absent array / nullable
  query, form and header fields decode to `[]` / `null`.
- `id_effect_axum::middleware`: `CompressionLayer` (gzip / brotli / deflate), `RequestIdLayer`
  (`x-request-id` propagation, `RequestId` extractor, one `http.request` effect span per request),
  `TimeoutLayer` (503 Problem; interrupts the handler's fiber), `BodyLimitLayer` (413 Problem) and
  `RateLimitLayer` (429 Problem + `Retry-After`; token buckets behind a pluggable
  `RateLimitStore`). New dependency `id_effect_resilience`. `RateLimitKey::tenant` keys limits by
  the request's tenant (read from `RequestCaps` or a request extension).
- `id_effect_axum`: `server::serve_dir` static files with strong `ETag` / `Last-Modified` / 304,
  `If-Match` / `If-Range`, precompressed `.br` / `.gz`, hashed-asset `Cache-Control`, traversal
  and hidden-file protection; `middleware::HttpCacheLayer` + `CacheControl` add `Cache-Control`
  and a body-hash `ETag` to GET responses and answer `If-None-Match` / `If-Modified-Since` with
  304. Book: part6/ch29-03-middleware.md; example `090_middleware`.
- `id_effect_axum` feature `openapi`: `OpenApi::from_router` generates an OpenAPI 3.1 document from
  typed routes. Component schemas are exactly the decoding schemas' JSON Schema; path / query /
  header parameters, request bodies per media type, documented and Problem
  (`application/problem+json`) responses, security schemes (bearer JWT, session cookie, OpenID
  Connect) with per-operation requirements from the new `RouteInfo::policies`.
  `Endpoint::from_parts` / `Endpoint::layer`; `id_effect_authz` guarded endpoints convert into an
  `Endpoint` and record their policies. Opt-in `openapi_router(&doc, OpenApiUi::scalar())` /
  `doc.routes()` serve `/openapi.json` (ETag, 304) and a Scalar `/docs` page loaded from a CDN or
  mirror (`cdn_url`, `integrity`) under its own strict CSP that works behind `SecurityLayer`;
  `route::introspect::routes_router` serves a `/_routes` table (method, path, handler, schemas,
  policies) as JSON or text; `RouteInfo::handler` records the handler's type name. Book:
  part6/ch29-02-openapi.md; example `085_openapi`.
- `id_effect_authz`: `Principal` and the `CurrentPrincipal` capability; `Policy<Res>` evaluated as an
  effect returning `Decision`, combined with `and` / `or` / `negate`; `Rbac` (roles → permissions,
  inheritance, wildcards); the named-policy registry `Policies` with `authorize` / `check`;
  ownership, tenant and ACL resource policies; claim (ABAC) policies; `AuthzError::http_status`
  (401 / 403 / 500). Feature `axum`: route guards (`Guard` layer; `.authorize(guard)` on an axum
  `MethodRouter` and on a typed `Endpoint`) answering 401 / 403 / 500 as `application/problem+json`.
  Every decision can be recorded as a serde `DecisionEvent` to the `AuthzEvents` sink capability.
  Feature `testing`: `AuthzTest` with `assert_allows` / `assert_denies` / `assert_denied_because`.
- `id_effect_auth`: argon2id `PasswordHasher` (OWASP defaults, rehash-on-verify, dummy verification
  for unknown users, hashing on the blocking pool) and server-side `Sessions` over a `SessionStore`
  (`MemorySessionStore`; idle and absolute timeouts, id rotation on login, rotate / logout /
  revoke-all, `__Host-` Secure HttpOnly SameSite=Lax cookie); feature `axum` adds `SessionLayer`.
  Credential middleware `Authenticate` (session → bearer JWT → API key → Basic, configurable
  order) installs a `CurrentPrincipal` (`AuthPrincipal`) for the authz guards; a bad credential is a
  401 Problem with `WWW-Authenticate`. JWT issue / verify (feature `jwt`: HS / RS / PS / ES / EdDSA,
  iss / aud / exp / nbf / leeway, required claims, claim → roles / permissions mapping);
  `JwksClient` with kid-rotation refetch, rate limiting and stale-cache fallback (feature `jwks`);
  hashed API keys with constant-time compare; Basic auth via `UserLookup` + `verify_or_dummy`.
  Feature `pg`: `PgSessionStore` keeps sessions in PostgreSQL (SHA-256 of the session id as the
  key, jsonb attributes, timestamptz expiries, upsert `save`), `pg_session_store_layer()`,
  `ensure_schema()` and the migration `migrations/0001_id_effect_sessions.{up,down}.sql`.
  Feature `oidc`: OpenID Connect login. `OidcClient` (discovery, authorization code + PKCE S256,
  state / nonce, ID tokens verified over the provider's JWKS with `iss` / `aud` / `exp` / `nonce`
  / `azp`, `ClaimMapping`, RP-initiated logout; implements `id_effect_platform`'s `OAuthClient`)
  and `OidcRoutes` (`/auth/oidc/{login,callback,logout}`, a separate flow session, login through
  `Sessions::login`). Tested against a wiremock IdP and a Keycloak container.
  Feature `totp`: TOTP second factor (totp-rs). Enrolment with an `otpauth://` URI, ±1-step
  verification on the executor clock with per-user replay protection, single-use recovery codes
  stored as argon2id hashes, and a session step-up with `amr` claims for the `require_mfa` authz
  policy.
- `id_effect_config`: HashiCorp Vault KV v2 secret provider behind the new `vault` feature
  (`VaultSettings`, `VaultProvider::layer`, `VaultAuth::token`). Secrets load once in a layer, come
  back as `Secret<T>`, and merge above the profile files and below the environment;
  `refresh_every` re-fetches and swaps atomically; failures are `ConfigError::Vault`.
  `Secret<T>` now implements `Deserialize`.
- `id_effect_sql`: typed SQL. `SqlValue` (bool, i16/i32/i64, f32/f64, text, bytes, uuid, jiff
  `Timestamp` / `DateTime` / `Date` / `Time`, `rust_decimal::Decimal`, JSON), `ToSqlValue` /
  `FromSqlValue`, `params!`, typed `SqlRow::get::<T>(name | index)` with errors naming the column,
  `SqlColumn` metadata, `Json<T>`. PostgreSQL binds NULL untyped and decodes by column type;
  unsupported column types are a clear error instead of a silent `""`. `testing::arb_sql_value`
  (feature `proptest`). Book: part6/ch28-01-typed-sql.md.
- `id_effect_sql`: `FromRow` and `#[derive(FromRow)]` (`rename`, `default`, `flatten`, `#[schema]`
  renames, `#[from_row(schema)]` validation into the new `SqlError::InvalidRow`), tuple `FromRow`,
  `query_as` / `query_one_as` / `query_opt_as`, `SqlError::RowNotFound`.
- `id_effect_sql`: nested `savepoint` scopes; isolation / read-only / deferrable transactions
  (`transaction_with`, `with_transaction_options`, `SqlClient::begin_with`);
  `retry_serialization_failures` over a `Schedule`; streaming queries (`query_stream`,
  `query_stream_as`) with bounded buffering and cancel-on-interrupt on PostgreSQL. `TestSqlClient`
  supports all three.
- `id_effect_sql` feature `migrate`: `Migrator` (`from_dir`, `from_files`, `from_sqlx` for
  `sqlx::migrate!` embedding) with `run` / `status` / `revert(n)` / `check` as effects and a startup
  `Layer`; sqlx-cli file naming and checksums, tracking table `_id_effect_migrations`, one
  transaction per migration under a PostgreSQL advisory lock with a timeout, typed `MigrateError`.
- `id_effect_sql` feature `sqlite`: `SqliteSqlClient` over sqlx SQLite (in-memory and file
  databases, savepoints, streaming, `Serializable` → `BEGIN IMMEDIATE`, busy → retryable 40001).
  `RoutingSqlClient` sends reads to replicas (round-robin or custom) and writes / transactions to
  the primary. One conformance suite runs on PostgreSQL and SQLite.
- `id_effect_sql` feature `mysql`: `MySqlSqlClient` / `MySqlPoolConfig` on sqlx; the shared
  conformance suite passes on MySQL 8. Isolation levels, `READ ONLY` transactions, savepoints,
  deadlocks and lock wait timeouts retryable as SQLSTATE 40001, bounded streaming with
  `KILL QUERY` on drop.
- `id_effect_sql` feature `query`: build statements with sea-query 1.0, render them per dialect
  (`BuildSql::build_sql(Dialect::{Postgres, MySql, Sqlite})` → SQL + `Vec<SqlValue>`, our own value
  bridge, no sea-query-binder) and run them with `execute_query` / `fetch_all_as` /
  `fetch_one_as` / `fetch_optional_as`.
- `id_effect_sql` feature `repository`: `#[derive(Entity)]` and `Repository<T>`. CRUD, paging,
  optimistic locking via `#[entity(version)]` (`SqlError::StaleVersion`), soft-delete / tenant /
  custom global filters on every generated statement with `with_deleted()` / `unscoped()` escape
  hatches (a tenant-scoped entity with no tenant fails closed); runs in transactions via
  `repo.on(tx)`. Book: part6/ch28-03-repositories.md; example `040_repository`.
- Book part6/ch30-07-audit-tenancy.md: the row-level tenant pattern end to end (authenticated
  principal → `CapOverrideLayer` setting `CurrentTenant` and `AuditContext` → tenant-filtered
  `Repository` → `audit::record` in the request's transaction → `verify_chain`), its fail-closed
  behaviour and the alternatives it does not cover (row-level security, schema or database per
  tenant); example `id_effect_sql/050_tenancy_audit`.
- `id_effect_sql` feature `audit`: hash-chained audit log. `AuditLog` layer, `append` / `record`
  inside the caller's transaction, one chain per tenant with `sha256(prev_hash ‖ record)` (the
  chain head is row-locked, so concurrent appends never fork it), `verify_chain` reports the first
  edited, removed or relinked record; bundled migration `migrations/audit/`.
- `id_effect_test`: effect-native test containers (testcontainers). Postgres (with a ready
  `SqlClient` layer under feature `sql`), MySQL, Redis, Keycloak, Vault and a generic
  `ContainerSpec` as scoped layers, removed when their scope closes (success, failure, defect or
  interrupt); `Shared` one-per-binary containers with the Ryuk reaper; readiness waits with typed
  timeouts (`ContainerError::NotReady`); `require_docker()` prints SKIP, or fails under
  `ID_EFFECT_REQUIRE_DOCKER=1`. Book: part4/ch15-07-containers.md.
  Per-test database isolation: `isolated_db()` (a rolled-back transaction over any `SqlClient`,
  with savepoints per statement and per nested transaction) and `TemplateDb` (a migrated template
  cloned per test with `CREATE DATABASE … TEMPLATE`, dropped on scope close); `Fixture` /
  `Factory` with seeded Schema-arbitrary defaults and `FromRow` rows (feature `fixtures`); an
  in-process `TestClient` over any tower `Service` (feature `http`). Book:
  part4/ch15-08-integration-testing.md; example `020_isolated_db`. Feature `contract`: `Contract` /
  `TestResponse::assert_conforms` check responses against the response schema the OpenAPI document
  declares for their operation and status, naming each violation by JSON pointer.
- `id_effect_kit`: features `openapi`, `sql`, `sql-sqlite`, `sql-mysql`, `migrate`, `repository`,
  `auth` (+ `auth-oidc`, `auth-totp`), `authz`, `redis`, `config-vault` and `test` (for
  dev-dependencies; not in `full`); `full` updated; the prelude gains typed routes, `Problem`,
  `Valid*`, middleware, `OpenApi`, `FromRow` / `Entity` / `Repository`, `Migrator`, the auth and
  authz types and `Profiles`. Derives used through the kit alone need
  `#[from_row(crate = id_effect_kit::sql)]`.
- `id_effect_tool` `service` template: applies `migrations/` with `id_effect_sql::migrate::run` on
  start (no more `include_str!` + execute on every start), serves a typed Problem+JSON route
  behind request-id / compression / timeout / body-limit middleware, and with `--db pg` its CI
  runs `id-effect migrate run` + `migrate check`.
- `id_effect_tool`: `id-effect migrate new|run|status|revert|check` (sqlx-cli file layout;
  sequential or timestamp numbering following the directory; `DATABASE_URL` / `.env`; `--json`;
  exit codes 0 / 1 / 2 / 3). `check` exits 1 on pending, drifted or missing migrations, for CI.
  Book: part6/ch28-02-migrations.md; example `id_effect_sql/examples/030_migrate.rs` (in memory by
  default, live on PostgreSQL with `DATABASE_URL` / `--live`).

### Changed

- **Breaking** `id_effect_sql`: statements take `&[SqlValue]`; rows are typed; `SqlError` is
  `#[non_exhaustive]`. See [MIGRATION-0.9.md](docs/MIGRATION-0.9.md).
- **Breaking** `id_effect_tool`: `cli::Command` gains `Migrate` and is `#[non_exhaustive]`.
- `id_effect_sql`: the backends enable sqlx's `time` feature (sqlx 0.8.6 or later), so sqlx's
  `query!` macros type date-time columns as `time` types even when `chrono` is also enabled. See
  [MIGRATION-0.9.md](docs/MIGRATION-0.9.md).

### Deprecated

- `id_effect_sql::SqlParam`, `SqlRow::new`, `SqlRow::text_cells` (removed in 0.10).
- `id_effect_axum::json::decode_json_schema`, `JsonSchemaError`, `SchemaJsonErrorBody` (removed in
  0.10; see [MIGRATION-0.9.md](docs/MIGRATION-0.9.md)).

### Security

- `id_effect_sql`: TLS for PostgreSQL and MySQL (rustls on aws-lc-rs, no OpenSSL); 0.8 connected in
  plaintext. `PgPoolConfig` / `MySqlPoolConfig` take `with_tls_mode(PgTlsMode / MySqlTlsMode)`,
  `with_root_cert_file` / `with_root_cert_pem` and `effective_tls_mode()`, overriding the URL's
  `sslmode` / `ssl-mode`. The default stays `Prefer` / `Preferred` (unverified, may fall back to
  plaintext); use `VerifyFull` / `VerifyIdentity` in production. With sqlx 0.8, `VerifyCa` also
  checks the host name (stricter than its name).
- `id_effect_config`: `ProfiledConfig` and `FigmentConfigProvider` no longer print merged values in
  `Debug`, and errors from `Config::secret()` loaders no longer contain the raw value.
- `id_effect_node`: the handshake docs no longer call the QUIC transport mutual TLS (no client
  certificate; the server certificate is verified only with pinned roots).

### Scorecard

Measured by blind re-score (`docs/scorecard/REPORT-0.9.0.md`, METHOD 2.1):

| Index | 0.8.0 | 0.9.0 | Target |
|---|---|---|---|
| Enterprise (E) | 55.2 (#5) | 67.2 (#3) | 64.6 (#3) |
| Runtime (RT) | 64.4 (#2) | 70.8 (#2) | 68.9 (#2) |
| Rows < 40 | 31 | 21 | ≤ 21 |
| Rows where id_effect leads | 7 | 19 | — |

24 rows up, none down. Data 39.7 → 79.6 (ahead of every competitor), Security 48.2 → 87.8. Every
0.9-targeted row met or beat its target except `dx.scaffold` (75; the generators moved to 0.10).
DX KPIs: K3 (lines to add the invoice resource to Ledger) 508, K1 67.6 s, K4 17/20, K5 94.7 %.

## 0.8.0 — 2026-10-16 — Trust & first hour (breaking)

First release under the plan of plans (`.maestro/specs/roadmap/PLAN-OF-PLANS.md`). Upgrading
from 0.7: see the [0.8 migration guide](docs/MIGRATION-0.8.md).

**License: MIT from 0.8.0** (was CC-BY-SA-4.0; versions up to 0.7.0 keep their license). A
`LICENSE` file, `license = "MIT"` in every manifest, and a DCO sign-off for contributions
(`CONTRIBUTING.md`). The labs `id_effect_jobs` obix outbox adapter was removed before the switch.

### Added

- **Trust & supply chain:** `SECURITY.md` (private reporting via GitHub advisories, response
  targets, support window), `GOVERNANCE.md`, `CODEOWNERS`. CI runs `cargo deny check`
  (`deny.toml`) and a report-only `cargo semver-checks` against 0.7.0 (enforcing from 0.8.0).
  Releases attach a CycloneDX SBOM per crate and the published `.crate` files with build
  provenance attestations. `docs/STABILITY.md` states the semver, deprecation and MSRV policy.
- **Web security (`id_effect_axum`):** `SecurityLayer::recommended(key)` — per-session CSRF
  tokens in a signed `__Host-` cookie with constant-time compare (on by default), nonce-based CSP
  (`CspNonce`, script hashes), HSTS / X-Frame-Options / nosniff / Referrer-Policy defaults, and
  `Cookies` with secure defaults and signed/private jars keyed by a `CookieKey` effect layer.
  `CorsConfig` builds a validated `tower_http` CORS layer.
- **Metrics:** `id_effect::observability::prometheus` renders a `MetricRegistry` as Prometheus
  text 0.0.4 (no dependency); `runtime_metrics` adds `id_effect_fibers_live` and
  `id_effect_fibers_started_total`. `id_effect_axum::metrics` serves `GET /metrics` and records
  HTTP requests and durations by route template (`http_metrics_layer()`).
  `id_effect_sql::metrics::InstrumentedSqlClient` records database operations and durations;
  `record_pg_pool` samples pool gauges.
- **Health:** `HealthRegistry` of effect-backed `HealthIndicator`s, run concurrently with
  per-check timeouts, answers `/ready` with JSON `{status, checks}` and 200 or 503; cache and
  `ReadinessCheck` adapters; `id_effect_sql::health::sql_ping` and `pg_pool_health`.
- **CLI (`id_effect_cli`):** `run_main` handles Ctrl-C gracefully (first SIGINT interrupts so
  finalizers run, exit 130; a second exits at once). New `completions` feature
  (`completions::{generate, CompletionsArgs, Shell}`); `examples/cli-minimal` gains
  `completions <shell>`.
- **Compile time:** `scripts/compile-time.sh` measures the template app's cold build, rebuild and
  boot; 0.8 figures in `docs/benchmarks/compile-0.8.json`.
- **Cron jobs:** `Cron` expressions with time-zone/DST-aware next-fire, `WallClock`,
  `Schedule::cron`; named `CronJob`s with overlap and missed-run policies, success/failure hooks
  and run history, declared in a `CronRegistry` and run directly or through a `Layer` that
  provides a `CronScheduler`. All of it runs on virtual time under `TestExecutor`.
- **Resilience (`id_effect_resilience`):** a `Fallback` policy and `Pipeline`, which composes
  fallback, retry, rate limiter, bulkhead, circuit breaker and timeout into one reusable policy
  with `PipelineEvent` listeners and a per-pipeline metric.
- **Streams:** `zip` / `zip_with` / `combine_latest`; `throttle` / `throttle_enforce` /
  `debounce` on the fiber clock; `group_by_key` / `group_by` substreams with bounded buffers and
  backpressure. Stopping early interrupts upstream and runs its finalizers.
- **Schema:** JSON Schema (2020-12) export for any `Schema` or `HasSchema` type
  (`schema.json_schema()`, `json_schema_for::<T>()`) with constraints, annotations and `$ref`
  recursion, read from the same schema value that validates. `decode_unknown_all` returns every
  error in one pass. Built-in validators `email`, `uuid`, `url`, `length`, `range`, `pattern`,
  `one_of` and friends.
- **Testing:** `run_test`, `assert_exit_*` / `expect_exit_*` and golden files (`assert_golden*`,
  `ID_EFFECT_BLESS=1`). The `proptest` feature returns as `testing::property`: schema-derived and
  framework-type generators, effect-aware properties and model-based testing (`StateMachine`).
  `TestExecutor::seeded(seed)` explores fiber interleavings reproducibly;
  `explore_interleavings` / `assert_interleavings` report the failing seed.
- **Book:** an idiom and style guide (appendix E); its snippets and the interleavings chapter's
  run as doctests.
- **`id_effect_kit` (new crate):** one dependency for apps, with feature bundles `web`,
  `sql-pg`, `otel`, `cluster`, `full`, a `prelude`, whole-crate re-exports and `graph`
  (`LayerGraph`: a layer dependency graph as text or DOT that flags missing providers).
- **`id_effect_tool` (new crate), the `id-effect` binary:** `id-effect new <name> --template
  service|worker|cli|cluster [--db pg] [--otel]` generates a project on the kit with tests, a
  Dockerfile, compose, `.env.example`, a CI workflow and (service) Kamal config; secure defaults,
  `/health`, `/ready` and `/metrics` are wired in. `id-effect doctor [--json] [--graph]` checks
  the toolchain, duplicate id_effect versions, env keys, database reachability, docker and the
  port. CI generates, builds and tests every template and docker-builds the service.
- **Diagnostics:** a gallery of the 20 most common mistakes (`tests/ui/gallery`,
  `docs/diagnostics.md`); 17 of their first errors now state the fix (capability, `~` bind,
  error-conversion and `caps!` errors).
- **Schema:** `#[derive(Schema)]` types export JSON Schema (`$ref`/`$defs` for nested types);
  `schema-regex` feature with a `regex` validator.
- **Docs:** a zero-to-production tutorial; 95 % of the book's Rust snippets now compile in CI
  (`examples/book-snippets`); `scripts/dx-kpis.sh` measures the first-hour KPIs (new service to
  `/health` in about 19 s cold).

### Changed

- **Shutdown:** every host shutdown path marks readiness `shutting_down` before draining, and
  `run_until_shutdown` keeps serving while it drains.
- **Streaming:** `Sink::fold_left` / `from_fold` / `collect` / `drain` / `collect_to_map` return
  `FoldSink`; `FoldSink::zip` and `FoldChannel::from_sink` reject non-fold sinks at compile time
  instead of panicking (`.into()` converts to `Sink`).
- **`RunMainConfig`** is `#[non_exhaustive]` with builder methods.

### Deprecated

- `CsrfConfig::new`, `csrf_middleware`, `csp_middleware`, `set_csrf_header` (removed in 0.9);
  the deprecated CSRF compare is now constant-time.

### Performance

- `Queue::offer` / `take` and `Channel::write` / `read` complete synchronously when they can;
  `retry` / `repeat` read the clock only for schedules that use it. Against 0.7.0:
  `coord/queue_bounded_10k` 0.48×, `coord/channel_spsc_10k` 0.50×,
  `bench_schedule_retry_overhead` 588 → 137 ns. All three are now budget gates.

### Scorecard

Measured by blind re-score (`docs/scorecard/REPORT-0.8.0.md`, METHOD 2.1):

| Index | 0.7.0 | 0.8.0 | Target |
|---|---|---|---|
| Enterprise (E) | 43.2 (#9) | 55.2 (#5) | 53.0 (#6) |
| Runtime (RT) | 56.1 (#6) | 64.4 (#2) | 63.3 (#3) |
| Rows < 40 | 39 | 31 | ≤ 31 |

27 rows up, 1 down (`msg.brokers`: the obix outbox was removed for the MIT relicense). One miss
carried over to 0.9: `data.jsonschema` 38 vs 50.

## 0.7.0 — 2026-10-15 — The runtime release (breaking)

Replaces the `!Send`, busy-polled kernel with an owned fiber runtime on an `Executor` trait
(tokio is the default backend), makes `Effect` `Send` with a typed `R`, adds the Effect.ts
parity layer and prunes the 0.6 deprecations. See
[ADR 0011](docs/adrs/0011-fiber-runtime-send-typed-r.md). Upgrading from 0.6: see the
[0.7 migration guide](docs/MIGRATION-0.7.md), which lists every removal with its replacement.

### Changed

- **Kernel:** `Effect<A, E = Never, R = HNil>` is `Send`; `LocalEffect` / `effect_local!` hold
  `!Send` values. Fibers have real wakers, interruption runs finalizers, and a parent interrupts
  its children when it exits. Effects run once: `retry`, `repeat` and `Layer::effect` take a
  factory.
- **Typed `R`:** capabilities are an HList (`HCons` / `HNil`, `caps!`, `need::<K>()`, `~K`); a
  missing capability is a compile error. `provide`, `provide_env`, `provide_all` and `narrow`
  replace `Env`, `Needs` and `Cap`.
- **Run edges:** `Effect::block_on`, `Runtime::run(..).await` and `Runtime::fork` return `Exit`.
  A blocking edge inside an async context refuses to run (`BlockOnInAsync`).
- **`effect!`** binds sequentially; `~(a, b, ..)` joins in parallel (0.6 parallelised
  independent binds implicitly).
- **`Cause::Die`** carries a `Defect` that keeps the panic payload; panics in fibers,
  finalizers and edges surface as `Die`.
- **Coordination and resources** (`Ref`, `Semaphore`, `Queue`, `Deferred`, `Latch`, `PubSub`,
  `Channel`, `Pool`, `Cache`, …) have plain constructors and suspend on wakers. A bounded
  `Queue::offer` waits; `KeyedPool` capacity is per key; `Cache` caches typed failures.
- **Streams** are owned pulls; `map_effect` is sequential (`map_effect_par(n, f)` for
  concurrency).
- **`Concurrency::Inherit`** takes its limit from the ambient `Fabric`, admitted asynchronously.
- **Satellite crates** are on the 0.7 kernel; their providers are `Layer`s. Highlights:
  `id_effect_axum` runs one fiber per request (panic → 500, disconnect → interrupt);
  `id_effect_cli::run_main` maps defects to 101 and interrupts to 130; `id_effect_config` reads
  only through `Config<T>` and `Config::all` accumulates errors; `id_effect_sql` transactions
  roll back on defects and interrupts and statements run on the transaction's connection;
  `id_effect_process` processes are fibers; `id_effect_node` waits are effects;
  `id_effect_resilience::hedged` is a race; `id_effect_fp` STM retry parks instead of spinning.
- **Labs** crates are migrated to the 0.7 kernel without new features.

### Added

- `Layer` values: `succeed`, `effect`, `scoped`, `acquire_release`, `merge`, `provide_to`,
  `provide_merge`, `fresh`, `build_scoped`, `launch`; `Effect::provide_layer`.
- `zip_par`, `zip_with_par`, `all`, `for_each_par`, `fork_all` with `Concurrency`; `race`,
  `race_all`, `race_first`, `timeout`, `timeout_fail`; `fork_daemon` inherits `FiberRef`s.
- Typed `Schedule<Out, In>` with `Effect::retry` / `retry_or_else` / `repeat` / `repeat_or_else`.
- `Effect::request` with `RequestResolver::make_batched` batching and `RequestCache`.
- `Random` / `TestRandom`; `TestExecutor` (virtual time); `TokioExecutor`.
- Metrics on atomics (`Counter`, `Gauge`, `Histogram`, `Summary`, `Frequency`) in a
  `MetricRegistry`; `tag_metrics`; spans with `SpanHook`; `tracing_hook()` for the in-process
  tracing buffer.
- Schema `tuple5`..`tuple12` and `struct5`..`struct12`; `Config::<T>::layer()`.
- `id_effect_axum::server_layer` / `serve*`, `id_effect_sql` scoped client and transaction
  layers, `id_effect_opentelemetry` registry bridge and `OtelSpanHook`.
- `examples/http-showcase` (axum + sql pg + config + otel, unpublished).

### Removed

- Crates `id_effect_tower` and `id_effect_sql_pg` (use `id_effect_axum` feature `tower`,
  `id_effect_sql` feature `pg`).
- `StreamV1`, `ComputeSupervisor`, `fabric.supervisor()`, `id_effect_platform::process`,
  `#[derive(SchemaParser)]`, `#[capability]`, the root `pipe1/2/3`, `memoize`, `flip`, `const_`,
  `tupled`, `untupled`, `MutableRef`, and the module aliases `id_effect::{channel, ref_, either,
  func, clock, duration, schedule, data, stream, snapshot}`.
- The 0.6 kernel: `run_blocking` / `run_async` / `run_fork` / `run_test*`, `Env` / `Cap` /
  `Needs`, the provider graph (`ProviderSpec`, `ProviderBox`, `build_env`, `run_with`), the
  `provide!` / `providers!` / `require!` / `mock_capability!` macros, `CancellationToken`,
  `Clock` / `TestClock`, the free `retry` / `repeat`, the 0.6 `Metric`, and the
  `testing::{snapshot, proptest, stream}` helpers with the `proptest` feature.
- The `id-effect-diagnose` binary of `id_effect_cli`.

### Fixed

- The request resolver awaits its fetch (no `run_blocking`) and fails entries with a typed
  error instead of panicking.
- `#[derive(Schema)]` compiles for `i32`, `Option<bool | f64 | i32>`, `Vec<i32>` and
  `Vec<T: HasSchema>` fields.
- `Stm::or_else` no longer leaks the retrying branch's writes into the alternative.
- `id_effect_jobs`' `apalis` feature compiles against `apalis-postgres` 1.0.0.

## 0.6.0 — 2026-10-14 — Consolidation (breaking)

Narrows the workspace to its core purpose and hardens what remains. See
[ADR 0010](docs/adrs/0010-crate-tiers.md) and [docs/QUALITY_BAR.md](docs/QUALITY_BAR.md).
0.5.0 was never published; 0.6.0 is the next crates.io release after 0.4.0.
Upgrading from 0.4 or 0.5: see the [0.6 migration guide](docs/MIGRATION-0.6.md).

### Runtime fixes

- `run`, `run_with`, `ThreadSleepRuntime::default()` and the default `TokioRuntime`
  keep a caller-installed Compute Fabric. They used to replace it. They now call the
  new `ensure_default_fabric()`. `install_default_fabric()` still overwrites, and its
  docs say so.
- Telemetry is lazy and never sleeps. `SysinfoTelemetry` creates its
  `sysinfo::System` on the first snapshot, which returns a neutral CPU reading.
  Later samples reuse the cached reading until enough real time has passed. A cold
  `run_with` takes about 1 ms (was about 400 ms); the first Tokio spawn takes about
  3 ms (was about 200 ms).
- New: `ensure_default_fabric`, `installed_fabric::<E>()`, `is_fabric_installed`,
  `uninstall_fabric`, `SysinfoTelemetry::new` / `is_initialized`,
  `ComputeFabric::telemetry`.
- **Breaking:** `SysinfoTelemetry` is no longer a unit struct. Use
  `SysinfoTelemetry::new()`.

### Streams are pull-based

- `map`, `map_serial`, `filter`, `filter_serial`, `take`, `take_while`, `drop_while`,
  `grouped`, `scan`, `map_effect`, `state_scan`, `transduce_items` /
  `via_transducer`, all windows, and `merge` / `combine_latest` / `keyed_join` now
  emit incrementally and work on infinite sources. Previously each one collected its
  whole input first.
- `from_iterable`, `range`, `unfold` and `unfold_effect` produce lazily.
- **Breaking:** `Stream::from_iterable` requires `I::IntoIter: 'static`.
- New `Stream::from_pull` constructor and `PullFn` alias. `Stream::new` and
  `Stream::from_effect` are documented as single-batch.
- `take(n)` never asks upstream for more than it still needs. `take_while` stops at
  the first false.
- Streams end at the first error: the next poll returns `Ok(None)`. This includes
  `chain`, which used to re-poll the failed side.
- `from_channel` and `from_duplex_queue_channel` read one item per pull.
- Fixed: `merge` dropped the rest of the longer side after 64+ elements.
- Fixed: `Channel::from_stream` applied `flat_map_out` only to the first element of
  each chunk. The rest were buffered unchanged, so `[1,2,3]` through
  `x ↦ [x,-x]` read as `1,2,3,-1`.
- `combine_latest` takes one chunk from each side per pull. It used to drain the
  whole left side first.
- `keyed_join`'s memory grows with the number of distinct keys, as documented.
- Time windows emit once the largest timestamp seen passes the window end.
  - `tumbling_by_time`: an item arriving after its bucket was emitted forms a new
    bucket.
  - `sliding_by_time`: items older than an already-emitted window start are dropped.
- Pulls respect a cancelled `CancellationToken` and yield to the executor while
  upstream keeps returning empty batches.
- `map_effect` runs nothing until the first pull.
- New test helper `id_effect::testing::assert_bounded_memory`.

### Workspace restructure (ADR 0010)

- **Labs:** `id_effect_rpc`, `id_effect_ai`, `id_effect_parse`, `id_effect_lint`,
  `id_effect_workflow`, `id_effect_events`, `id_effect_jobs` and `id_effect_graph` move
  to `labs/`. This is a separate Cargo workspace with `publish = false`. These crates
  are no longer published. They are built and tested by their own CI job, and main CI
  no longer builds rdkafka, OpenSSL, bundled SQLite or apalis.
  - `id_effect_jobs` now uses the stable apalis 1.0 family.
- **Merged:** `id_effect_tower` becomes `id_effect_axum::service` (feature `tower`).
  `id_effect_sql_pg` becomes `id_effect_sql::pg` (feature `pg`). The old crates are
  re-export shims for 0.6 only and will be removed in 0.7.
- `id_effect_fsm` no longer depends on `id_effect_workflow`. The durable bridge
  (`register_fsm`, `step_durable`, `restore_state`, `FsmSnapshot`, `WorkflowFsmError`)
  moves to `id_effect_workflow::fsm` (labs).
- `LogLevel` moves into core (`id_effect::LogLevel`); `id_effect_logger` re-exports
  it, and `id_effect_config` no longer depends on `id_effect_logger`.
- `id_effect_optics::transducer` re-exports core's transducer instead of keeping a
  duplicate. Core gains `transducer::take`.
- **Removed:**
  - `#[derive(Fsm)]` and `id_effect::Fsm` (a stub)
  - the generated `T::parser()` from the schema derive (use `id_effect_parse`'s
    `SchemaBridge::parser_for::<T>()`)
  - the empty `algebra` feature
  - the `id-effect` scaffolder binary and `id_effect_cli::generator` (stubs; the
    `id-effect-diagnose` binary stays)
- **Added:**
  - `#[derive(Schema)]` / `id_effect::Schema`. It now also supports `Option<T>` and
    `Vec<T>` fields.
  - `id_effect_platform` feature `test-util`, which `MemoryOAuthClient` now requires.
- **Renamed (old names deprecated):**
  - `ComputeSupervisor` is now `FabricGovernor`, and `ComputeFabric::supervisor()` is
    now `governor()`
  - `id_effect_platform::process` is now `command`
  - `SchemaParser` is now a deprecated alias of `Schema`
- **Changed:** `Monad::flatten` has a real default implementation and no longer calls
  `unimplemented!`.
- **Publishing:** publish order lives in one place
  (`scripts/validate-crate-packaging.sh --print-order`), and `publish.yml` loops over
  it.

### Slimmer core and the new `id_effect_fp` crate (breaking)

- New maintenance-tier crate **`id_effect_fp`**. It takes the code core does not use
  itself: `algebra`, `collections` (persistent structures), `stm`,
  `kernel::{result, reader, thunk}`, and the unused `foundation` pieces (`unit`,
  `never`, `function`, `product`, `coproduct`, `isomorphism`, `option_`). These paths
  move:
  - `id_effect::{algebra, collections, stm}` → `id_effect_fp::{algebra, collections, stm}`
  - `id_effect::kernel::{result, reader, thunk}` → `id_effect_fp::{result, reader, thunk}`
  - Root re-exports such as `EffectHashMap`, `EffectVector`, `RedBlackTree`, `Trie`,
    `MutableList`, `ChunkBuilder`, `Stm`, `TRef`, `TMap`, `atomically`, `Foldable`,
    `Alternative` and `traverse_vec` → `id_effect_fp::…`
- Placement moves to **`id_effect_node::placement`**: `PlacementMode`,
  `ClusterResourcePolicy`, `NodeCandidate`, `LoadReport`, `place`,
  `pick_placement`, `score_candidate` and `ClusterTelemetry`.
  `FabricGovernor::scale_out_placed` becomes the free function
  `id_effect_node::scale_out_placed`.
- New default-on **`compute`** feature: it gates `compute::*` and parallelism, and
  brings in rayon and sysinfo. With `--no-default-features`, core builds without
  rayon, sysinfo or im, and streams, sorts and runtimes run serially.
- `im` is no longer a dependency. The following now use std collections:
  `SpanRecord::annotations`, `TracingFiberRefs::span_annotations`,
  `Metric::snapshot_frequencies`, `Sink::collect_to_map` (`HashMap`) and
  `TimeBucketMap` (`BTreeMap`).
- tokio stays a required dependency (features `macros`, `rt`, `sync` only), because
  the coordination primitives are built on `tokio::sync`.
- Core no longer has a dev-dependency on `id_effect_config`.

### Hardened runtime crates

- **`id_effect_process`:**
  - A stopped process's mailbox is closed on every exit path. Sending to a dead pid
    now returns `SendError::Rejected`; it used to succeed silently.
  - A panic in a handler, `init` or `terminate` is caught. The process exits with
    `ExitReason::Error("panic: …")` and goes through the normal exit path:
    links and monitors fire, pending calls fail with `CallError::Down`, and the
    supervisor restarts it. Before, it never finalized and callers timed out.
  - New `Addr<DynamicSupervisor>::terminate_dyn_child`.
  - Tests went from 23 to 80.
- **`id_effect_fsm` (breaking):**
  - `Interpreter::step` and `run` take the machine by value and return an `Effect`
    yielding the updated machine. The new state is kept only if the action succeeds.
  - `Saga::run` and `compensate_indices` return `Effect`s.
  - Session types are removed (`SessionSend`, `SessionRecv`, `PingPong`, …).
  - `#[derive(Fsm)]` rejects duplicate edges even when the same event is spelled
    two ways (`Go` and `E::Go`).
  - New: a real `#[derive(Fsm)]` (`#[fsm(event = …, initial = …)]`,
    `#[fsm(on(Ev => Target))]`) with compile-time checks, and a gen_statem-style
    process behaviour (`FsmProcess`, `FsmSpec`, `FsmHandle`). fsm now depends on
    `id_effect_process`.
- **`id_effect_node`** (tests went from 30 to 203; bugs found while testing):
  - Call/spawn replies and monitor `Down` messages were broadcast to every peer, so
    one node's reply could complete another node's request. They now go only to the
    sender (new `Mesh::handle_envelope_from`).
  - Monitoring an unreachable node now fires `NoConnection`. Demonitor works (new
    `Mesh::remote_demonitor`). Pending requests are cleaned up on failure or timeout.
  - A child whose takeover fails is kept as orphaned and retried instead of being
    dropped (`DistributedSupervisor::orphaned` / `retry_orphaned`).
    - Placement tries the preferred node and then the local node.
    - Retries run in the background (`on_membership` now returns
      `Option<RetryHandle>`) and skip the departed node until it rejoins.
  - Monitors that peers hold on our processes are keyed by the peer's name and boot
    incarnation, and are removed when the peer goes down. A restarted node can no
    longer receive another incarnation's `Down`.
  - Monitor sinks run after the lock is released, so a sink can call
    `remote_monitor` or `remote_demonitor` without deadlocking.
  - In-memory transport:
    - `close()` closes both ends, and a dropped listener frees its address.
    - Frames that already arrived are delivered after a close or partition.
    - Connections killed by a partition stay dead after `heal`.
  - QUIC: it used to panic whenever it built a TLS config (two crypto providers were
    linked). It now uses an explicit provider and TLS 1.3 only. The server name is
    configurable (`QuicConfig::with_server_name`, default `"cluster"`).
  - **Breaking:** `PgAdvisoryLease` lock keys are now derived with SHA-256 (they
    used to come from std's hasher, which can change between Rust versions). Do not
    run old and new builds against the same singleton.

### Capability ergonomics

- New `Effect::widen_error::<E>()`: lifts an effect whose error is `()`, `Never` or
  `Infallible` into any error type `E`, with no `map_error(|()| unreachable!())`. On a
  real error type it is a compile error (sealed `kernel::InfallibleError`).
- New `Effect::into_env::<R>()` for `R = ()` effects. `~` binds any `()`-environment
  effect in any environment.
- **Breaking:** `EffectLogger` methods (`info`, `debug`, …) no longer take a type
  parameter. They return `Effect<(), EffectLoggerError>`, so write
  `~logger.info("…")` inside `effect!`, or `.into_env()` elsewhere. An explicit
  turbofish no longer compiles.
- `#[provides(Service, with = || …)]` (or `with = |deps: &Env| …`, or
  `with = Self::build`) defines a provider without a hand-written constructor, so
  `#[allow(clippy::new_ret_no_self)]` is no longer needed.
- `type AppCaps = caps!(A, B);` aliases work in `effect!(|r: &mut AppCaps| …)`.
- Capability diagnostics:
  - `#[track_caller]` on `Env::get`, `try_get`, `get_any`, `Needs::need`.
  - **Breaking:** `try_get` and `get_any` misses return
    `CapabilityError::MissingAt { cap, location }`. Match both shapes with
    `err.missing_capability()`.
  - `CapabilityId::name()` and `Display` show service names.
  - `#[diagnostic::on_unimplemented]` hints on `Needs`, `CapabilityKey`,
    `CapabilitySet`, `FromEnv`, `CapKeys`, `HasCap`, `CapWiden` and `CapBind`.
- `#[capability]` is documented as the deprecated no-op it has been since 0.3. It
  will be removed in 0.7.

### Integrations

- **`id_effect_axum` (breaking):**
  - Effects run on an `EffectExecutor`, which replaces `block_in_place`. It is an
    elastic pool of dedicated threads: each in-flight request gets its own thread with
    a private single-threaded runtime. Handler futures are `Send`, a blocking effect
    stalls only its own request, and current-thread runtimes work.
    - Bounded admission: `max_threads` (default 512) plus `queue_capacity` (default
      1024).
    - `SaturationPolicy::Wait` (default) or `Reject`, which answers 503 with
      `Retry-After`.
    - `DropPolicy::RunToCompletion` (default): an effect finishes even if the client
      disconnects. `DropPolicy::Cancel` is opt-in.
    - Configure it with `EffectExecutor::builder()`. A service can take its own
      executor via `EffectService::with_executor`.
  - `run_with_env` and `run_with_caps` now require `Send + 'static` closures and
    `Send` result and error types.
  - `tokio::spawn` inside an effect runs on an executor worker.
  - New request-scoped capability overrides: `CapOverrideLayer`, `RequestCaps`,
    `execute_with_request_caps`.
- **`id_effect_platform`:** the `auth` module is behind an opt-in `auth` feature.
  reqwest 0.13 only.
- **`id_effect_sql`:** live Postgres tests fail when `ID_EFFECT_REQUIRE_PG=1` and no
  database is available (CI's new `pg-integration` job sets it). Otherwise they print
  `SKIP`; they no longer pass silently.
- **`id_effect_cli`:** new example `010_exit_codes`. `examples/cli-minimal` is the
  template, and `scripts/check-version-drift.sh` (run in CI) catches stale
  `id_effect` version pins.
- **`id_effect_resilience`:**
  - New `retry_with_breaker`: retries on a `Schedule` but fails fast when the breaker
    is open.
  - Clock injection: `CircuitBreaker::make_with_clock`, `RateLimiter::make_with_clock`.
  - Bulkhead's docs now explain that it rejects instead of waiting.
- Every published crate has a README with an example and passes the
  quality-bar check for its tier (`scripts/tier-conformance.sh`).

### Tooling

- MSRV is declared: `rust-version = "1.94"`, checked by a CI `msrv` job.
- Every crate declares its tier in `[package.metadata.id_effect]`.
  `scripts/tier-conformance.sh` checks the quality bar for that tier.
- `scripts/readme-crates.sh` generates the README crate table.

## 0.5.0 — Process Mesh & distributed runtime (never published)

Semver-minor release introducing the **Process Mesh**: a BEAM/OTP-style local
process runtime (`id_effect_process`) and a distribution layer (`id_effect_node`)
for node identity, membership, remote processes, and distributed supervision.
Existing cluster/supervision/jobs/workflow stubs are wired into the mesh behind a
single placement scorer. See [ADR 0009](docs/adrs/0009-process-mesh.md) and
[book Part VII](crates/id_effect/book/src/part7/ch36-00-process-mesh.md).

### Added

- **New crate `id_effect_process`** — typed mailboxes, GenServer-shaped behaviors,
  links, monitors, `trap_exit`, exit-signal propagation, and multi-strategy OTP
  supervision trees, deterministic under `TestClock`.
- **New crate `id_effect_node`** — node identity, cookie-authenticated handshake,
  pluggable transports (InMemory/TCP/QUIC), SWIM membership, remote processes,
  process groups, and distributed supervision.
- **Unified placement API** — `id_effect::place` is the single pure entry point,
  fed by `LoadReport` (mesh telemetry DTO) with a `NodeCandidate` adapter. New
  `PlacementMode::RoundRobin` (deterministic rotation) joins `Affinity`,
  `LocalFirst`, and `Spread`.
- **Placement-aware distribution** — `mesh` gains `report_load` / `load_reports` /
  `remote_spawn_placed`; `DistributedSupervisor` places children via
  `id_effect::place`, honoring `spec.prefer` and excluding downed nodes on takeover.
- **Node-aware jobs** — `FabricJobRunner` honors `JobSpec.target_node` via
  `dequeue_for` / `pending_for`; `ComputeSupervisor::scale_out_placed` targets flow
  through to the runner.
- **Distributed workflow journal** — `RemoteStepJournal` backs `DistributedStepJournal`
  over a `JournalTransport` (`JournalServer` + `LoopbackTransport`) with a wire
  protocol and `WorkflowError::Transport`. `NetworkJournalStub` is retained as a
  test double.
- **`PgAdvisoryLease`** (feature `pg`) — `SingletonLease` backed by Postgres
  session-scoped advisory locks on a dedicated worker connection (CP production
  path). `MemoryLease` retained for tests.
- **`concurrency::RestartIntensity`** — shared sliding-window restart-intensity
  primitive (distinct from `RestartWithLimit`'s total-count cap).
- Examples: `132_distributed_supervisor_failover.rs`, `133_cluster_stream_fanout.rs`.

### Changed

- Capability API uses **service type names** directly: `caps!(Counter)`, `require!(Counter)`, `#[provides(Counter)]`.
- `Cap<T>` replaces generated `*Key` types; `#[capability]` is a no-op (removed from public flow).
- Trait-backed services use type aliases (`HttpClientService`, `ConfigProviderService`, …).
- `PlacementMode::LocalFirst` and `Spread` now diverge — previously they scored
  identically. `LocalFirst` prefers the least-loaded node; `Spread` picks lowest raw load.
- `id_effect_process` OTP supervisor delegates restart-intensity accounting to the
  shared `concurrency::RestartIntensity` primitive (behavior preserved).
- **Workspace versioning** — the root manifest owns `[workspace.package]`
  (version/edition/license/repository); every publishable member inherits via
  `version.workspace = true`, so future bumps propagate atomically.

### Removed

- Public `*Key` types and `define_capability!`.

### Version alignment

| Crate | Version |
|-------|---------|
| `id_effect` | **0.5.0** |
| `id_effect_process` | **0.5.0** (new) |
| `id_effect_node` | **0.5.0** (new) |
| workspace adapters | **0.5.0** |

## 0.4.0 — Implicit parallelism (breaking)

Semver-major release collapsing caller-facing parallelism policy into **Compute Fabric**. See [ADR 0008](docs/adrs/0008-implicit-parallelism.md) and [book ch13-05](crates/id_effect/book/src/part4/ch13-05-parallelism.md).

### Breaking

- **Removed public `Parallelism`** and all `*_with(policy, …)` dispatch on collections, `vec`, `order`, and `Stream`
- **Removed deprecated `*_par`**, `Stream::map_par_n`, `Stream::map_par_adaptive`, and public `compute::effective_threshold`
- Bulk and stream-chunk ops no longer accept caller policy — Fabric's `should_parallelize_current` is the sole threshold

### Added

- `compute::dispatch::parallel_if_profitable` — central Fabric-aware Rayon dispatch
- `Stream::map_effect` — admission-bounded concurrent effect mapping per chunk
- EDG parallel codegen for independent bind sets (`join_binds2` / `join_binds3` / `join_binds4`)
- Default Compute Fabric installation in `run()` and `run_with()`; `ensure_run_context()` at run boundaries
- Book rewrite: [Implicit Parallelism](crates/id_effect/book/src/part4/ch13-05-parallelism.md); Compute Fabric cross-refs in [ch12](crates/id_effect/book/src/part3/ch12-00-compute-fabric.md)
- Example: `122_compute_fabric_effect_parallel.rs` (`Stream::map_effect` under Fabric)

### Changed

- Primary `map`, `filter`, `map_values`, `sort_with`, and related bulk APIs parallelize implicitly when Fabric says it is profitable
- `effect!` may run independent `~` binds concurrently when the EDG finds no data or capability conflict
- `*_serial` and `#[effect(serial)]` remain the explicit escape hatches for `FnMut`, non-`Send`, and ordering

### Migration (from 0.3.x parallel-by-default)

| Old | New |
|-----|-----|
| `Parallelism::…` | removed — trust Fabric |
| `map_with(Parallelism::Serial, f)` | `map_serial(f)` |
| `map_with(Parallelism::ForceParallel, f)` | `map(f)` |
| `*_par(f)` | `map(f)` |
| `Stream::map_par_n(n, f)` | `map_effect(f)` |
| `Stream::map_par_adaptive(f)` | `map_effect(f)` |

Prior [ADR 0006](docs/adrs/0006-parallel-by-default.md) bulk behavior is retained internally; the public policy surface is superseded by [ADR 0008](docs/adrs/0008-implicit-parallelism.md).

### Version alignment

| Crate | Version |
|-------|---------|
| `id_effect` | **0.4.0** |
| workspace adapters | **0.4.0** |

## 0.3.0 — DI maturity (breaking)

Semver-major release completing capability-first DI adoption. See [appendix-b-migration.md](crates/id_effect/book/src/appendix-b-migration.md) and [ADR 0004](docs/adrs/0004-provider-parity-and-cap-subtyping.md).

### Removed

- `CapEnv1…CapEnv6` — use `CapList` / `caps!(K0, K1, …)` only
- `#[capability]` / `*Key` types — use service names with `Cap<T>`
- `require!(env, K)` — use `require!(K)` inside `effect!` or `Needs::<K>::need(env)`
- `ctx!`, `req!`, `service_key!`, `Layer`, `Stack`, `Effect::provide`, `IntoBind` (legacy paths)
- `id_effect_config::ambient` — use `Env::scoped` / `build_env`

### Added

- `CapList` + unbounded `caps!` arity
- `Cap<T>`, `#[derive(ProviderSpecDerive)]`, `#[provides(Service)]`, `#[named("variant")]`
- `CapWiden` for capability-set subtyping
- `ProviderSpec::optional_requires`, `shared()`, `refresh_interval()`, `on_refresh()`
- Fiber/request scoped overrides: `with_override`, `with_fiber_and_override`
- `id-effect-diagnose manifest` with TOML/JSON + `--json`
- Examples: `042_effectful_config_provider`, `043_named_variant_providers`, `id_effect_axum` `020_capability_run_with`
- Expanded trybuild corpus (12 cases) under `tests/ui/`

### Version alignment

| Crate | Version |
|-------|---------|
| `id_effect` | **0.3.0** |
| `id_effect_platform` | **4.0.0** |
| workspace adapters | **0.3.0** |

## Pre-0.3 history — "2.0.0" capability-first DI

Internal milestone numbered "2.0.0" before the workspace settled on 0.x semver. It was
never published under that number; its changes shipped as part of 0.3.0 above.
