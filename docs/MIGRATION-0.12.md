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

## HTTP client: `HttpClientConfig::http3`

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

## RPC: OpenAPI output is 3.1 (was 3.0)

## Templates: `server.http3` in the `service` template

## Semver report: each reported break and its entry
