# Migrating to id_effect 0.12

This guide is the "if you used X, now do Y" companion to the `0.12.0` section of
[CHANGELOG.md](../CHANGELOG.md). It assumes you are on 0.11. If you are on 0.10, apply
[MIGRATION-0.11.md](MIGRATION-0.11.md) first.

Every intentional public-API break in 0.12 is listed here, each with its shim (rule L9) or the
reason it has none. A break reported by `cargo semver-checks` (the `semver` CI job, baseline the
last 0.11.x release) that is not listed below is a bug. The deprecation rule and semver policy are
in [STABILITY.md](STABILITY.md).

0.12 is the "one call surface" release (ADR 0014): RPC is the first-party call surface, REST +
OpenAPI stays the public API style, and HTTP/3 is a standard transport with TCP fallback.

The guide is complete against `cargo semver-checks` 0.50.0 run on every publishable crate,
`release/0.11.0` against the 0.12.0 release candidate (the last section maps each reported break
to its entry). There was no GraphQL surface in 0.11 (no crate, module or feature), so 0.12 removes
none; ADR 0014 records GraphQL as a non-goal.

## Upgrade checklist

Ordered by how likely a 0.11 application is to hit it.

1. **RPC clients and scripts send `x-rpc: 1` and a JSON content type on every `POST`** (RPC:
   the 0.12 wire format). A `POST` to `/rpc` without `x-rpc: 1` is `403`, without a
   `Content-Type` (or with another one) `415`. `RpcClient`, the generated Rust and TypeScript
   clients and `curl` examples in the book send both.
2. **Struct literals of `HostConfig`, `HttpClientConfig`, `RpcClientConfig` or `DevArgs`** no
   longer compile (each gained public fields): end the first two with `..Default::default()`,
   build an `RpcClientConfig` with `RpcClientConfig::new(..)`, and add the three new `DevArgs`
   fields.
3. **`id_effect_node::transport::certs` imports** move to `id_effect_platform::tls` (deprecated
   re-export, removed in 0.13).
4. **A QUIC mesh upgrades all nodes together**: 0.12 nodes require ALPN `idm/1` and refuse 0.11
   nodes. TLS-TCP meshes are unaffected.
5. **HTTP/3 binds UDP on the TCP port** when the host has TLS from certificate files and feature
   `http3`; open UDP on the firewall or set `HTTP3=false`.
6. **Labs `id_effect_rpc` users** depend on the published crate and port `RpcGroup::register` /
   `RpcClient::call` to the 0.12 signatures, or to `#[rpc::service]`.
7. **Exhaustive `match`es on `RpcClientError`** get the new variants or a wildcard.
8. **Code that relied on `UnwindSafe` / `RefUnwindSafe`** for `HostConfig` or `QuicTransport`
   (`catch_unwind` across them) wraps them in `AssertUnwindSafe`.
9. **Generated OpenAPI consumers** read OpenAPI 3.1 (component names changed).
10. **The 10,000-stream soak** (`streams_soak`, bench job) gates RSS growth from minute 5, not
    minute 1.

## Deprecated for one minor (removed in 0.13)

| Deprecated | Use instead | Since |
|---|---|---|
| `id_effect_node::transport::certs::{NodeCertificate, CertReloader, ReloadTask, CertError, load_ca_bundle, ca_bundle_from_pem}` | `id_effect_platform::tls::…` (the same types) | 0.12.0 |
| `kit::node::transport::certs` | `kit::platform::tls` | 0.12.0 |

Every other break has no deprecated path; each section below gives its shim (most often
`..Default::default()` or a builder) or says why there is none.

## Node: `transport::certs` moved to `id_effect_platform::tls`

The certificate code (`NodeCertificate`, `CertReloader`, `ReloadTask`, `CertError`,
`load_ca_bundle`, `ca_bundle_from_pem`) moved to `id_effect_platform::tls` (feature `tls`), so the
QUIC endpoint shared with HTTP/3 and the HTTP servers and clients use the same types (ADR 0014
D6). `id_effect_node`'s `quic` and `tls-tcp` features now depend on `id_effect_platform`.

**Shim:** `id_effect_node::transport::certs` is a `#[deprecated(since = "0.12.0")]` module that
re-exports the same types for one minor (removed in 0.13). They are the same types, so values
move freely between the two paths.

```rust,ignore
// before
use id_effect_node::transport::certs::{CertReloader, NodeCertificate, load_ca_bundle};
// after
use id_effect_platform::tls::{CertReloader, NodeCertificate, load_ca_bundle};
```

Also public now: `tls::Identity` (the swappable certificate a listener presents),
`client_tls_config`, `server_tls_config`, `server_tls_config_with`, `client_cert_verifier` with
`ClientAuth::{Required, Optional}`, `provider` and `sha256`. `CertError` gained the variants
`Tls` and `ClientRoots` (it is `#[non_exhaustive]`).

`cargo semver-checks` reports the six items as removed from `id_effect_node::transport::certs`
(`struct_missing`, `enum_missing`, `function_missing`) because it does not follow re-exports of
another crate's items; the old paths still compile (test
`deprecated_node_certs_path_still_compiles` in `crates/id_effect_node/tests/shared_endpoint.rs`),
with a deprecation warning.

`QuicTransport` is no longer `UnwindSafe` / `RefUnwindSafe`: it holds the platform
`QuicEndpoint` (quinn and rustls state behind locks). **No shim**; code that moves a transport
into `std::panic::catch_unwind` wraps it in `AssertUnwindSafe`.

## Axum: TLS and HTTP/3 server configuration fields

`HostConfig` has two new public fields: `tls: Option<HostTls>` (default `None`: plain HTTP, as in
0.11) and `http3: bool` (default `true`; it only has an effect with feature `http3`). A struct
literal that lists every field no longer compiles. **Shim:** end the literal with
`..HostConfig::default()`, which keeps 0.11's behaviour. `HostConfig::config()` also reads
`TLS_CERT_FILE`, `TLS_KEY_FILE`, `TLS_RELOAD_SECS` and `HTTP3`; setting only one of the two TLS
files is a config error.

```rust,ignore
// before
HostConfig { bind_host, bind_port, shutdown_timeout, load_shed }
// after
HostConfig { bind_host, bind_port, shutdown_timeout, load_shed, ..HostConfig::default() }
```

`HostConfig` is no longer `UnwindSafe` / `RefUnwindSafe`, because `HostTls::Provided` holds a
`ServerTls` (an `Arc<rustls::ServerConfig>`, whose certificate resolver is an `Arc<dyn …>`). **No shim**: a `catch_unwind` that
captures a `HostConfig` wraps it in `AssertUnwindSafe`.

`ConnectionLimits` gained `tls_handshake_timeout` (10 s) and `max_concurrent_streams` (100,
HTTP/2) with their `dangerous_*` opt-outs; its fields are private, so no code breaks. One
behaviour change: `max_requests_per_connection` now also ends an HTTP/2 connection (a `GOAWAY`
after the n-th request; the streams already open finish). In 0.11 the HTTP/2 path existed only
through feature unification and had no limit.

With `tls` set, a binary built without the axum feature `tls` refuses to start (0.11 had no TLS
field, so nothing breaks; this guards against serving plain HTTP by accident).

With feature `http3`, `tls` from certificate files (`TLS_CERT_FILE` / `TLS_KEY_FILE`) and
`http3` left on, the host entry points also bind **UDP on the TCP port number** and serve the
same router over HTTP/3, and TCP responses carry `Alt-Svc: h3=":<port>"; ma=86400`. A host
whose UDP port is taken fails to start (`HostError::Serve("bind udp …")`). **Shim:** `HTTP3=false`
(`HostConfig::http3 = false`) restores the TCP-only 0.11 behaviour; open UDP on the firewall
otherwise. `HostTls::Provided` settings serve HTTP/3 when they carry an identity
(`ServerTls::from_identity`, `ServerTls::from_pem_files`, or `ServerTlsBuilder::identity` for a
custom resolver); without one they serve TCP only (logged), or use `server::serve_h3` on an
endpoint of your own.

HTTP/3 connections run **BBR** congestion control (`Route::h3()`), mesh connections CUBIC (ADR
0014 D9). quinn marks its BBR experimental; on an endpoint of your own pick per route with
`Route::with_congestion_control(CongestionControl::Cubic)` (the axum host and `serve_h3` register
`Route::h3()` and so run BBR). The QUIC endpoint asks for 4 MiB UDP socket buffers; on Linux
raise `net.core.rmem_max` and `net.core.wmem_max` (for instance to 4194304) or the request is
capped (logged at `debug`). `QuicEndpoint::stats()` returns `EndpointStats`, new in 0.12 and
`#[non_exhaustive]`: read its fields, never build one (it gained `congestion_bbr` and
`congestion_cubic` during 0.12, and later fields do not break readers). Nothing in 0.11 had it.

## HTTP client: `HttpClientConfig::http3`

`HttpClientConfig` gains two public fields: `root_certificates` (extra CA roots, DER, trusted on
TCP and HTTP/3) and, with the platform feature `http3`, `http3` (`Http3Config`). A struct literal
that names every field no longer compiles.

**Shim:** none needed for the builder style. Build the config from its default and the `with_*`
methods, or end a literal with `..HttpClientConfig::default()`:

```rust,ignore
// before
let config = HttpClientConfig { connect_timeout, timeout, retry, ssrf, max_redirects };
// after
let config = HttpClientConfig { connect_timeout, timeout, retry, ssrf, max_redirects, ..HttpClientConfig::default() };
```

HTTP/3 is off for the client by default. `HttpClientConfig::default().http3(true)` makes
`ReqwestHttpClientProvider::layer_with` build an `Http3Client` (book chapter 29.8); without it
nothing changes.

Behaviour change: the platform HTTP client (reqwest, and the TCP path of `Http3Client`) now
negotiates **HTTP/2** by ALPN with `https` servers that offer it (reqwest feature `http2`); 0.11
spoke HTTP/1.1 only. Plain `http://` stays HTTP/1.1. No shim is needed; a server that
misbehaves on HTTP/2 can be pinned to HTTP/1.1 by not offering `h2` in its ALPN.

## Node: the QUIC mesh requires ALPN `idm/1`

`QuicTransport` now runs on an `id_effect_platform::quic::QuicEndpoint` and offers and requires
ALPN `idm/1` (`transport::quic::ALPN_MESH`). A 0.11 node offers no ALPN, so its handshake with a
0.12 node fails: upgrade every node of a QUIC mesh together (mixed-version meshes are a non-goal;
RPC clients cross versions through contract versions). TLS-TCP meshes are unaffected.

The endpoint turns on QUIC address validation (Retry; one extra round trip per new connection),
keeps 0-RTT off, and bounds streams, windows, the idle timeout (30 s, as before) and handshakes
(`QuicLimits`). Raising a limit is `QuicEndpointConfig::dangerous_raise_limits`; turning Retry off
is `dangerous_no_retry`; both are recorded in the posture registry.

New, optional: `QuicTransport::on_endpoint(config, &endpoint)` puts the mesh on an endpoint shared
with HTTP/3 (one UDP port). There client certificates are optional in the handshake and the
`idm/1` route closes every connection without a verified client chain before reading it (ADR
0014 D7). `QuicSettings { mesh_port }` (the `quic.mesh_port` config key) keeps the mesh on its own
port with handshake-level mutual TLS, which is also what `QuicTransport::new(..).listen(addr)`
does. **No shim** is needed for code that only uses `QuicTransport::new` / `try_new`: it keeps its
own endpoint and the 0.11 behaviour.

The listener of a QUIC mesh runs handshakes in the background now (at most 256 at once, 10 s
each); a refused attempt still surfaces as `TransportError::Connect` from `Listener::accept`.

## RPC: labs `RpcGroup::register` becomes `#[rpc::service]`

`id_effect_rpc` left `labs/` and is published (tier `integration`). Depend on it from crates.io or
the workspace path `crates/id_effect_rpc` instead of `labs/id_effect_rpc`.

The low-level `RpcGroup` now requires an access decision per procedure (ADR 0014 C1). **No shim**:
an undecided procedure would be an open endpoint, which is what the rule removes.

```rust,ignore
// before (labs)
group.register("greet", payload, &["name", "n"], success, &["message", "n"], handler);
let app = RpcServer::new(group).mount_default();
let out = client.call("greet", input, &payload, &["name", "n"], &success, &["message", "n"]);
// after
group.register(Procedure::query("Greeter.greet").public(), payload, success, &["message", "n"], handler);
// or: Procedure::mutation("Greeter.reset").guard(staff_guard)
let app = RpcServer::new(group).mount_default(); // or RpcServer::new(group).mount(typed_router)
let out = client.call("Greeter.greet", input, &payload, &["name", "n"], &success);
```

- The input field names of `register` and the output field names of `RpcClient::call` are gone
  (they were never read).
- `RpcServer::mount_dispatch(axum::Router, path)` is replaced by `RpcServer::with_path(path)` and
  `mount(id_effect_axum::route::Router)`; `layer_rpc` takes the typed router. A plain
  `axum::Router` gets the route through `RpcServer::router().into_axum()` or `mount_default()`.
- An unknown tag and a malformed envelope answer `application/problem+json` (404 / 400) instead
  of the `RpcEnvelope`; `RpcClientError::Rpc` carries the whole `RpcEnvelope` (was its message).
- `id_effect_rpc::codegen` is removed (`emit_service_trait` emitted source strings); its
  `RpcServiceDef`, `RpcMethodDef`, `RpcHttpMethod` moved to `id_effect_rpc::openapi`.
  `#[rpc::service]` (r12-e12-1-macro) generates the handler trait, client and contract instead.
- `RpcClient` is behind the default feature `client`.

With the macro, the procedures move into a trait and the group, server, client and contract are
generated (book chapter 33.4):

```rust,ignore
#[rpc::service(version = 1)]
trait Greeter {
  #[query]
  #[public]
  fn greet(&self, input: GreetIn) -> Effect<GreetOut, Never, DynEnv>;
}
let app = GreeterService::server(MyGreeter).mount_default();
let client = GreeterService::client(HttpTransport::new(RpcClient::new(url), http_client));
let out = client.greet(input); // Effect<GreetOut, RpcCallError<Never>, R>
```

## RPC: the 0.12 wire format, procedure kinds and typed errors

The HTTP path speaks the 0.12 wire format (design note `r12-rpc-design.md`). **No shim**: one
server cannot answer both formats on the same route, and the 0.11 format was labs-only.

```text
before: {"tag":"Greeter.greet","payload":…}  →  {"kind":"success","tag":…,"success":…}
after:  {"id":7,"tag":"Greeter.greet","payload":…}  →  {"id":7,"tag":…,"exit":{"_tag":"Success","value":…}}
        failure: {"exit":{"_tag":"Failure","error":{"_tag":"NotFound","message":"…"}}}
```

- Every `POST` must send `Content-Type: application/json` (NDJSON for a client stream) and
  `x-rpc: 1` (`RpcClient` and the generated clients do). This is stricter than ADR 0014 rule C5,
  which asks for both on cookie-authenticated mutations: it applies to **every** `POST`, with
  or without a cookie — queries,
  batches, streams — because one route serves them all and a cross-site form can reach any of
  them. A `POST` without `x-rpc: 1` answers `403`; one **without a `Content-Type`**, or with
  another one, answers `415` (Problem Details). `curl` scripts add
  `-H 'content-type: application/json' -H 'x-rpc: 1'`. `GET /rpc/{tag}?input=<json>` serves
  queries without either.
- `RpcWireRequest`, `RpcWireResponse`, `id_effect_rpc::stream` and the `serialization` helpers
  `decode_wire_request`, `encode_wire_response`, `rpc_error_to_wire`, `success_wire`,
  `success_json`, `serialization_to_rpc_error`, `validate_success_json` are removed: use
  `wire::{WireCall, WireResult, WireExit, Frame}`.
- `RpcClientError::Rpc(RpcEnvelope)` is replaced by `RpcClientError::Failure(serde_json::Value)`
  (the `{"_tag": …}` error; `failure_tag()`, `reserved()`) and `RpcClientError::Refused(Problem)`.
- `RpcError` handlers keep working through `RpcGroup::register`; their errors are the union
  `InvalidArgument | NotFound | AlreadyExists | Unauthenticated` with a `message`.
  `RpcError::internal(..)` and every defect answer `InternalError { id }` (the correlation id):
  the message is logged, never sent.
- A payload that fails its Schema is `422` `InvalidInput { issues }` (every issue, JSON Pointer
  paths), no longer `400` `invalid_argument`.
- `RpcServer::endpoint()` is `endpoints()` (`POST /rpc`, `POST /rpc/{tag}`, `GET /rpc/{tag}`);
  `RpcMethodEntry::dispatch` is gone — call `server::dispatch::{dispatch, open, start_*}`.
- New procedures use the per-kind registration methods (`RpcGroup::query`, `mutation`,
  `server_stream`, `client_stream`, `bidi`, `subscription`) with `WireSchema` and a typed error
  implementing `RpcErrorUnion`.

## RPC: batches, and `RpcClient::call` batches the calls of a tick

- `POST /rpc` with a JSON array body is now a batch (0.12 before this answered `400`). Each
  call needs a distinct `id`; at most 50 calls and 1 MiB (`BatchConfig`).
- `RpcClient::call` waits for the batch window of its tick (a few executor yields) and may share
  a request with other calls; a lone call is still a plain envelope. Use
  `RpcClient::call_unbatched` for the 0.11 behaviour.
- `handler::CallContext` has a new public field `request_cache`: build it with
  `CallContext::new(..)` (and `.with_request_cache(..)`), not a struct literal.
- Behind `RateLimitLayer`, a batch of N calls takes N tokens: raise the burst to at least the
  largest batch you send (a batch larger than the burst is always `429`).
- Custom `RateLimitStore`s get `acquire_n` / `try_acquire_n_now` with defaults (one `acquire`
  per token, not atomic); override them to take N tokens at once.

## RPC: stream transports in the client and on `GET /rpc/{tag}`

`RpcClientConfig` has a new public field `transport: TransportKind` (default `Http`), so a struct
literal that lists every field no longer compiles. `RpcClientError` has a new variant `Transport`,
which breaks exhaustive `match`es. **Shim:** build the config with `RpcClientConfig::new(..)`
(optionally `.with_transport(..)`), or add `transport: TransportKind::Http` to the literal; give a
`match` a `Transport(_)` arm or a wildcard.

```rust,ignore
// before
RpcClientConfig { base_url, dispatch_path }
// after
RpcClientConfig { base_url, dispatch_path, transport: TransportKind::Http }
```

With feature `sse`, `GET /rpc/{tag}` that sends `Accept: text/event-stream` for a subscription or
server stream gets an event stream. Before, it got 406 or 405. Without that header nothing
changes. **No shim:** a client that relied on the refusal must drop the `Accept` header.

## RPC: OpenAPI output is 3.1 (was 3.0)

`id_effect_rpc::openapi` reads the service contract now. The 0.11 route-metadata emitter
(`OpenApiSpec`, `OpenApiInfo`, `RpcServiceDef`, `RpcMethodDef`, `RpcHttpMethod`,
`emit_openapi_json`, `emit_openapi_yaml`, OpenAPI 3.0.3) is removed without a shim: it described
hand-written route metadata, not the procedures `RpcServer` serves.

```rust,ignore
// 0.11
let json = emit_openapi_json(&OpenApiSpec { info, service: RpcServiceDef { service, methods } })?;
// 0.12
let json = id_effect_rpc::openapi::to_json(&Todos::document(), &OpenApiOptions::new("Todos API", "1.0.0"));
```

- The document is OpenAPI 3.1.0 (`jsonSchemaDialect` JSON Schema 2020-12). Component names
  change: contract types by the last segment of their Rust path, generated components with a
  `-` (`Todos-v1-get-Result`, `error-NotFound`, `rpc-Problem`).
- Server and client streams, two-way streams and subscriptions are described in the AsyncAPI 3.0
  document (`id_effect_rpc::asyncapi::to_json`).
- To serve the documents, merge `ContractDocs::new(contract).router()`. In `prod` add
  `.guard(g)`; unguarded, the routes are not mounted (ADR 0013 R7).
- Plain axum routes keep `id_effect_axum::openapi::OpenApi::from_router` (feature `openapi`).

## Templates: `server.http3` in the `service` template

New projects get it; an existing 0.11 service takes it in four steps:

- `Cargo.toml`: kit features `rpc` and `http3` next to `web`, and `id_effect_rpc` as a dependency
  (the `#[rpc::service]` expansion names it).
- `config/base.toml`: `[server.http3] enabled = true` (or `false` behind a load balancer that
  terminates TLS; `APP_SERVER__HTTP3__ENABLED=false` in the environment does the same).
  `src/lib.rs` `server_config()` applies it, and `[server.tls] cert_file` / `key_file` when the
  environment names no certificate; `main.rs` sets `host.config` to that before serving.
- The prod posture now refuses HTTP/3 without a certificate (probe "HTTP/3 only with TLS") and
  the `id-effect dev` certificate (probe "no dev certificate", C13): give prod certificate files
  or ACME, or turn HTTP/3 off. `config/deploy.yml` sets `APP_SERVER__HTTP3__ENABLED: "false"`
  for kamal-proxy, which terminates TLS.
- `id-effect dev` now issues a development certificate and passes `TLS_CERT_FILE` /
  `TLS_KEY_FILE` to the app (`--no-cert` keeps the 0.11 behaviour), and refuses to start with
  `ID_EFFECT_PROFILE` set to anything but `dev`.

The kit posture report has six more probes (`posture::PROBES` grows from 9 to 15); code that
matched on the list length or order updates. `kit::node::transport::certs` users (the 0.11
`cluster` template) import `kit::platform::tls` instead (`cluster` now exposes it).

## Tool: `id-effect dev` certificates and `id-effect rpc`

`id_effect_tool::dev::DevArgs` (the arguments of `id-effect dev`, public for embedding the
command) gained three public fields: `cert_dir`, `no_cert` and `cert_only`. A struct literal
that lists every field no longer compiles. **Shim:** add `cert_dir: None, no_cert: true,
cert_only: false` to keep the 0.11 behaviour (no development certificate), or `no_cert: false`
to get one; parsing the command line with clap needs no change.

`id-effect rpc check` (new) refuses a breaking change to a published contract version. How it
classifies a change is in ADR 0014 §9 and book chapter 33.12. Two rules differ from a naive
reading of "narrower is breaking", on purpose (F47): **a narrowed output is compatible** (old
clients still read every value the new server sends), and **a removed input field is compatible
unless the input refuses unknown fields** (`additionalProperties: false`), because the server then
ignores what an old client still sends. A breaking change in a new contract version, with the old
one still served, is reported as `versioned` and passes. No shim: it is a new check; run it in CI
against your last release tag (chapter 33.12, "In CI").

## Performance gates

The 10,000-stream soak (`crates/id_effect_rpc/examples/streams_soak.rs`, the `soak` job of
`bench.yml`) now gates RSS growth from **minute 5** (half the run, at most 300 s; 30 s of the 60 s
variant) instead of minute 1: the resume replay buffers warm up for about five minutes, bounded
(F50/F60). The minute-1 figure is still reported (`rss_kib.at_minute_1`,
`growth_since_minute_1`; `scripts/security-kpis.sh` `rss_growth_minute_1`). A dashboard or
script that read `rss_kib.mark_s == 60` reads `mark_s` instead.

## Semver report: each reported break and its entry

`cargo semver-checks` 0.50.0 (rustdoc JSON of Rust 1.98.1; 0.50 cannot read the pinned 1.99's
format, and only the rustdoc JSON is compared), `--workspace --baseline-rev release/0.11.0`
(`037dffc4`), and `-p id_effect_rpc --baseline-root` the 0.11 `labs/` workspace for the moved
crate. 31 publishable crates: 30 checked (`id_effect_rpc_macros` is new and has no baseline),
25 without a break. Breaks in 5 crates, every one listed above:

| Crate | Lint | Items | Entry |
|---|---|---|---|
| id_effect_axum | `constructible_struct_adds_field` | `HostConfig.tls`, `HostConfig.http3` | Axum: TLS and HTTP/3 server configuration fields |
| id_effect_axum | `auto_trait_impl_removed` | `HostConfig` no longer `UnwindSafe` / `RefUnwindSafe` | same section |
| id_effect_platform | `constructible_struct_adds_field` | `HttpClientConfig.root_certificates`, `HttpClientConfig.http3` | HTTP client: `HttpClientConfig::http3` |
| id_effect_node | `struct_missing`, `enum_missing`, `function_missing` | `transport::certs::{NodeCertificate, CertReloader, ReloadTask, CertError, load_ca_bundle, ca_bundle_from_pem}` (re-exported, deprecated; the tool does not follow cross-crate re-exports) | Node: `transport::certs` moved |
| id_effect_node | `auto_trait_impl_removed` | `QuicTransport` no longer `UnwindSafe` / `RefUnwindSafe` | same section |
| id_effect_rpc (vs labs 0.11) | `module_missing` | `stream`, `codegen` | RPC: labs `RpcGroup::register`…; RPC: the 0.12 wire format |
| id_effect_rpc | `struct_missing` | `RpcWireRequest`, `stream::RpcStreamChunk`, `openapi::{OpenApiSpec, OpenApiInfo}`, `codegen::{RpcServiceDef, RpcMethodDef}` | the 0.12 wire format; OpenAPI output is 3.1 |
| id_effect_rpc | `enum_missing` | `RpcWireResponse`, `codegen::RpcHttpMethod` | same |
| id_effect_rpc | `function_missing` | `serialization::{decode_wire_request, encode_wire_response, rpc_error_to_wire, success_wire, success_json, serialization_to_rpc_error, validate_success_json}`, `stream::encode_stream_chunk`, `openapi::{emit_openapi_json, emit_openapi_yaml}`, `codegen::emit_service_trait` | same |
| id_effect_rpc | `inherent_method_missing` | `RpcMethodEntry::dispatch`, `RpcServer::mount_dispatch` | labs `RpcGroup::register`; the 0.12 wire format |
| id_effect_rpc | `method_parameter_count_changed`, `method_requires_different_generic_type_params` | `RpcGroup::register`, `RpcClient::call` | labs `RpcGroup::register` |
| id_effect_rpc | `enum_variant_added`, `enum_variant_missing` | `RpcClientError::{Failure, Refused, Transport}` added, `RpcClientError::Rpc` removed | the 0.12 wire format; stream transports |
| id_effect_rpc | `constructible_struct_adds_field` | `RpcClientConfig.transport` | RPC: stream transports |
| id_effect_tool | `constructible_struct_adds_field` | `DevArgs.{cert_dir, no_cert, cert_only}` | Tool: `id-effect dev` certificates |

Unlisted breaks: none. Not reported but listed because they change behaviour or are new public
surface within 0.12: `CallContext::request_cache` (batches), `x-rpc` on every `POST` and `415`
without a content type (F16), HTTP/2 on the platform client, the QUIC ALPN `idm/1`, the posture
probes, OpenAPI 3.1 output, `EndpointStats`, and the soak mark.
