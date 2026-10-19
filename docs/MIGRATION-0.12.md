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

> Status: skeleton (leaf `r12-t0-skeletons`). One heading per planned break; each leaf that ships
> the break fills in its section, and leaf `r12-t0-migration-guide` completes the guide.

## Upgrade checklist

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
otherwise. `HostTls::Provided` settings serve TCP only (logged); use `server::serve_h3` on an
endpoint of your own for HTTP/3 with a custom resolver.

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

- Every `POST` must send `Content-Type: application/json` and `x-rpc: 1` (`RpcClient` does);
  without them the server answers 415 / 403 Problem Details. `GET /rpc/{tag}?input=<json>` serves
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

## Semver report: each reported break and its entry
