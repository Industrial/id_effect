# Transport security: mutual TLS between nodes

Nodes talk over QUIC (feature `quic` of `id_effect_node`), which is TLS 1.3. Since 0.11 the
transport is **mutually authenticated by default**: every node presents a certificate when it
dials and when it accepts, and every node verifies the peer's. A peer without a client
certificate, or with one your CA did not issue, is refused during the TLS handshake, before it
can send a single mesh frame.

The cookie handshake still runs on top. TLS proves *which machine* is on the other end; the
cookie proves it belongs to *this cluster*, and its proof is bound to the TLS channel (the
exporter secret), so it cannot be relayed onto another connection.

## Certificates from PEM files

Issue each node a certificate from your cluster CA, valid for both server and client auth, with
the server name every node dials (`"cluster"` by default, or set one with
`QuicConfig::with_server_name`). Ship three files per node: the chain (leaf first), the private
key, and the CA bundle.

```rust
use id_effect_node::transport::quic::{QuicConfig, QuicTransport};
// Since 0.12 (`id_effect_node::transport::certs` is a deprecated re-export for one minor).
use id_effect_platform::tls::{NodeCertificate, load_ca_bundle};

# fn main() {}
# #[allow(dead_code)]
# fn node() -> Result<QuicTransport, Box<dyn std::error::Error>> {
let roots = load_ca_bundle("/etc/mesh/ca.pem")?;
let cert = NodeCertificate::from_pem_files("/etc/mesh/node.crt", "/etc/mesh/node.key")?;
let config = QuicConfig::from_certificate(&cert)?.with_client_verification(roots);
let transport = QuicTransport::try_new(config)?;
# Ok(transport)
# }
```

`from_pem_files` reads PKCS#8 (`PRIVATE KEY`), PKCS#1 (`RSA PRIVATE KEY`) and SEC1
(`EC PRIVATE KEY`) keys, and fails when the key does not belong to the leaf.
`with_client_verification` checks peers against the CA in both directions; use
`with_client_cert_roots` if clients are issued by a different CA.

## Without a PKI: pinned certificates

Small clusters can skip the CA: give each node a self-signed certificate
(`QuicConfig::self_signed`) and pin every peer's SHA-256 fingerprint
(`QuicConfig::cert_sha256`) with `with_pinned_peer_cert_sha256`, which pins it both as a server
to dial and as a client to accept. `with_pinned_server_cert_sha256` and
`with_pinned_client_cert_sha256` pin one direction. With roots and pins together, both checks
must pass.

## No trust configured, no traffic

| Missing | Result |
|---|---|
| Server trust (roots or server pins) | `QuicTransport::try_new` fails with `QuicConfigError::NoServerTrust`; a transport built with `new` refuses to dial. |
| Client trust (roots, client roots or client pins) | `listen` fails with `TransportError::Bind` naming `QuicConfigError::NoClientTrust`. |
| A trusted client certificate on an inbound connection | That connection's `Listener::accept` returns `TransportError::Connect`; keep accepting. |

## Rotating certificates

`CertReloader` loads the chain and key files and swaps them when their contents change. It
polls; there is no filesystem watcher.

```rust
use std::{sync::Arc, time::Duration};
use id_effect_platform::tls::CertReloader;
# use id_effect_node::transport::quic::QuicConfig;

# fn main() {}
# #[allow(dead_code)]
# fn node() -> Result<(), Box<dyn std::error::Error>> {
# let roots = id_effect_platform::tls::load_ca_bundle("/etc/mesh/ca.pem")?;
let files = Arc::new(CertReloader::new("/etc/mesh/node.crt", "/etc/mesh/node.key")?);
let _polling = files.spawn_polling(Duration::from_secs(30)); // stops when dropped
let config = QuicConfig::from_reloader(&files).with_client_verification(roots);
# let _ = config;
# Ok(())
# }
```

Every new connection, dialed or accepted, presents the certificate the reloader holds at that
moment, so a running listener picks up a renewed certificate without a restart. Connections
already open keep theirs. Replace the files atomically (write, then rename): a file that does not
parse, or a key that does not match the chain, is logged and the current certificate stays until
the next poll.

## Opt-outs are named and recorded

Two switches weaken the defaults. Each records itself in the posture registry
(`id_effect::observability::posture`, ADR 0013), so the prod posture check can refuse to start
with one the operator has not accepted.

| Opt-out | Posture id | What it reopens |
|---|---|---|
| `QuicConfig::dangerous_accept_any_server_cert()` | `id_effect_node::QuicConfig::dangerous_accept_any_server_cert` | Any on-path host can terminate the dialer's TLS session. |
| `QuicConfig::dangerous_no_client_auth()` | `id_effect_node::QuicConfig::dangerous_no_client_auth` | The listener accepts peers without a certificate; membership rests on the cookie alone. |

TCP has the same model (feature `tls-tcp`):

```rust
# use id_effect_node::transport::{tcp::TcpTransport, tls_tcp::TlsTcpConfig};
# use id_effect_platform::tls::{CertReloader, load_ca_bundle};
# fn main() {}
# #[allow(dead_code)]
# fn node() -> Result<TcpTransport, Box<dyn std::error::Error>> {
# let files = CertReloader::new("/etc/mesh/node.crt", "/etc/mesh/node.key")?;
# let roots = load_ca_bundle("/etc/mesh/ca.pem")?;
let tcp = TcpTransport::tls(TlsTcpConfig::from_reloader(&files).with_client_verification(roots));
# Ok(tcp)
# }
```

That carries the frames in TLS 1.3 with mutual auth, with the same certificate types, the same
trust options and opt-outs (posture ids
`id_effect_node::TlsTcpConfig::dangerous_*`), and a channel binding for the cookie proof. Its
listener runs TLS handshakes in the background with a timeout, so a silent peer cannot stall it.
Plaintext TCP exists only with the feature `insecure-plaintext-tcp`, as
`TcpTransport::dangerous_plaintext()` (posture id
`id_effect_node::TcpTransport::dangerous_plaintext`). Tests:
`crates/id_effect_node/tests/transport_tcp_tls.rs`.

## Shared endpoint

Since 0.12 the mesh speaks ALPN `idm/1` and runs on an `id_effect_platform::quic::QuicEndpoint`:
one UDP socket that routes connections by ALPN, with QUIC address validation (Retry) on, 0-RTT
off, and finite stream, window, idle and handshake limits (`QuicLimits`; raising one is
`QuicEndpointConfig::dangerous_raise_limits`, turning Retry off is `dangerous_no_retry`, both
recorded in the posture registry). A peer that offers no ALPN, or another one, fails the
handshake, so a 0.11 node cannot join a 0.12 QUIC mesh.

A node that also serves HTTP/3 can put both on **one port**:

```rust
use id_effect_node::transport::quic::{QuicSettings, QuicTransport};
use id_effect_platform::quic::{QuicEndpoint, QuicEndpointConfig, Route};
use id_effect_platform::tls::Identity;
# use id_effect_node::transport::{Transport as _, quic::QuicConfig};

# fn main() {}
# #[allow(dead_code)]
# async fn node(public_identity: Identity, mesh_config: QuicConfig) -> Result<(), Box<dyn std::error::Error>> {
let endpoint = QuicEndpoint::bind("0.0.0.0:443".parse()?, QuicEndpointConfig::new(public_identity))?;
let h3 = endpoint.register(Route::h3())?; // handed to the HTTP/3 server
let settings = QuicSettings::default(); // the `quic` config section; `mesh_port` unset
let (transport, listen_on) = settings.mesh_transport(mesh_config, &endpoint);
let listener = transport.listen(&listen_on).await?; // registers `idm/1` on the same port
# let _ = (h3, listener, QuicTransport::new);
# Ok(())
# }
```

rustls picks the client-certificate rule per endpoint, before the ALPN is known, so on the
shared port mutual TLS cannot be a handshake rule for `idm/1` alone (ADR 0014 D7). Instead:

- the handshake **requests** a client certificate as optional, with **no CA hints**;
- a certificate that is sent is still verified against the cluster CA (or pins), and one from
  another CA fails the handshake;
- the `idm/1` route **closes every connection without a verified client chain before any frame
  of it is read** (application close code `CLOSE_CLIENT_CERT_REQUIRED`, `0x1d7`); the mesh
  listener reports it as `TransportError::Connect` and never sees the connection;
- `h3` ignores client certificates, and each route presents its own certificate (the mesh one
  for `"cluster"`, the public one for browsers).

Browsers with installed client certificates may show a certificate picker on the shared port. To
keep handshake-level mutual TLS, set `quic.mesh_port`: the mesh then gets its own endpoint (only
`idm/1`, client certificates required in the handshake), exactly as in 0.11.

```toml
[quic]
mesh_port = 4370
```

Tests: `crates/id_effect_node/tests/shared_endpoint.rs`
(`idm_without_client_cert_refused_on_shared_endpoint`,
`idm_cert_from_other_ca_refused_on_shared_endpoint`, `alpn_routes_h3_and_idm`,
`mesh_port_splits_endpoint`); the 0.11 mutual-TLS tests in `transport_tls.rs` run on both shapes;
the endpoint itself in `crates/id_effect_platform/tests/quic_endpoint.rs`.

**Runnable example:** `cargo run -p id_effect_node --features quic --example 134_mtls_cluster`
(two nodes from PEM files, a node from another CA refused, a certificate rotated under a running
listener). Tests: `crates/id_effect_node/tests/transport_tls.rs`.
