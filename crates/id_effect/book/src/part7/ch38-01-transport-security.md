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

```rust,ignore
use id_effect_node::transport::certs::{NodeCertificate, load_ca_bundle};
use id_effect_node::transport::quic::{QuicConfig, QuicTransport};

let roots = load_ca_bundle("/etc/mesh/ca.pem")?;
let cert = NodeCertificate::from_pem_files("/etc/mesh/node.crt", "/etc/mesh/node.key")?;
let config = QuicConfig::from_certificate(&cert)?.with_client_verification(roots);
let transport = QuicTransport::try_new(config)?;
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

```rust,ignore
use std::{sync::Arc, time::Duration};
use id_effect_node::transport::certs::CertReloader;

let files = Arc::new(CertReloader::new("/etc/mesh/node.crt", "/etc/mesh/node.key")?);
let _polling = files.spawn_polling(Duration::from_secs(30)); // stops when dropped
let config = QuicConfig::from_reloader(&files).with_client_verification(roots);
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

```rust,ignore
let tcp = TcpTransport::tls(TlsTcpConfig::from_reloader(&files).with_client_verification(roots));
```

That carries the frames in TLS 1.3 with mutual auth, with the same certificate types, the same
trust options and opt-outs (posture ids
`id_effect_node::TlsTcpConfig::dangerous_*`), and a channel binding for the cookie proof. Its
listener runs TLS handshakes in the background with a timeout, so a silent peer cannot stall it.
Plaintext TCP exists only with the feature `insecure-plaintext-tcp`, as
`TcpTransport::dangerous_plaintext()` (posture id
`id_effect_node::TcpTransport::dangerous_plaintext`). Tests:
`crates/id_effect_node/tests/transport_tcp_tls.rs`.

**Runnable example:** `cargo run -p id_effect_node --features quic --example 134_mtls_cluster`
(two nodes from PEM files, a node from another CA refused, a certificate rotated under a running
listener). Tests: `crates/id_effect_node/tests/transport_tls.rs`.
