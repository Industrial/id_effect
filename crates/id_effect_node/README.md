# `id_effect_node`

The distribution layer of the [`id_effect`](https://github.com/Industrial/id_effect) Process
Mesh (ADR 0009). `id_effect_process` runs processes on one node; this crate connects nodes into
a cluster and lets processes spawn, message, call, link and monitor each other across it.

## What it provides

| Item | Role |
|------|------|
| `NodeId` | Wire identity: logical `name@host` plus a 128-bit boot incarnation |
| `Cookie`, `handshake` | Mutual HMAC-SHA256 challenge/response; the cookie never crosses the wire |
| `Transport` / `Connection` / `Listener` | Byte-frame pipes. `InMemoryTransport` (deterministic, scripted partitions), `TcpTransport` (length-delimited; TLS 1.3 with feature `tls-tcp`), `QuicTransport` (feature `quic`) |
| `transport::certs` | Node certificates from PEM files, CA bundles, `CertReloader` for rotation (features `quic`, `tls-tcp`) |
| `Session` | An authenticated, framed connection to one peer (`wire::Frame`, postcard, `PROTOCOL_VERSION` 3) |
| `Membership` | SWIM membership via [`foca`], sans-io: the caller owns time and the network. `TimerWheel` drives it in tests; `NodeMonitors` fans out `NodeUp` / `NodeDown` / `Rejoined` |
| `Mesh`, `BehaviorRegistry` | Remote spawn / cast / call / monitor / link over sessions or the in-process `MeshFabric` |
| `ProcessGroups` / `OrSwot` | AP process groups as an ORSWOT CRDT; `fanout_cast` sends to every member |
| `DistributedSupervisor` | Places children by load telemetry and re-places them when their host goes down |
| `ClusterSingleton` | At most one holder cluster-wide. `MemoryLease` for single-process tests; `PgAdvisoryLease` (feature `pg`) for production |
| `Discovery` | Join candidates: `StaticDiscovery`, `DnsDiscovery`, `ChainedDiscovery` |
| `control` | Serializable cluster status / admin DTOs (mounted by `id_effect_axum`) |

Everything that waits for an answer — `Mesh::remote_spawn` / `remote_call`,
`DistributedSupervisor::start_child` / `on_membership`, the `ClusterSingleton` lease — returns an
`id_effect::Effect`: `Send`, interruptible, with timeouts on the executor clock. Compose
them into a program, or run them at the edge with `block_on`.

Delivery between nodes is **at-most-once**, ordered per connection, and dropped on disconnect.

## Trust model

The network transports are encrypted and mutually authenticated by default (ADR 0013 R8). QUIC
(`QuicConfig`) and TCP (`TcpTransport::tls` with a `TlsTcpConfig`) both run TLS 1.3 with mutual
TLS: every node presents a certificate in both directions and verifies its peer's, against the
cluster CA (`with_client_verification(load_ca_bundle(..)?)`) or pinned SHA-256 fingerprints
(`with_pinned_peer_cert_sha256`). There is no default trust: without roots or pins a dialer
refuses to dial and a listener refuses to bind. Certificates load from operator-issued PEM files
(`NodeCertificate::from_pem_files`), and a `CertReloader` rotates them for new connections
without a restart.

On top of TLS, the cookie handshake proves cluster membership; its proof is bound to the TLS
channel (exporter secret), so a man-in-the-middle cannot relay it. The handshake itself
authenticates but does not encrypt.

Every weakening is a named opt-out recorded in the posture registry
(`id_effect::observability::posture`): `dangerous_accept_any_server_cert()` and
`dangerous_no_client_auth()` on either TLS config, and `TcpTransport::dangerous_plaintext()`,
which exists only with the feature `insecure-plaintext-tcp`: frames then cross the network
unencrypted, and an on-path host can read, relay or inject them. `Cookie::dangerous_allow_short()`
likewise records itself.

## Feature flags

| Feature | Enables |
|---------|---------|
| `quic` | `transport::quic::QuicTransport` (quinn + rustls, mutual TLS) |
| `tls-tcp` | `TcpTransport::tls` and `transport::tls_tcp::TlsTcpConfig` (tokio-rustls, mutual TLS) |
| `insecure-plaintext-tcp` | `TcpTransport::dangerous_plaintext()` (unencrypted, recorded opt-out) |
| `kubernetes` | Kubernetes discovery from the in-cluster API |
| `pg` | `PgAdvisoryLease`: Postgres session advisory locks for `ClusterSingleton` |

## Quick start: two nodes over the in-memory transport

```rust
use id_effect_node::wire::Frame;
use id_effect_node::{Cookie, MemoryNetwork, NodeId, Session, Transport};
use id_effect_process::NodeName;

#[tokio::main(flavor = "current_thread")]
async fn main() {
  // One shared network per test; each node gets its own endpoint.
  let net = MemoryNetwork::new();
  let a = net.transport("a");
  let b = net.transport("b");
  // At least 32 bytes; shorter cookies fail every handshake.
  let cookie = Cookie::try_new("cluster-secret-of-at-least-32-bytes").expect("long enough");
  let id_a = NodeId::fresh(&NodeName::new("a@local"));
  let id_b = NodeId::fresh(&NodeName::new("b@local"));

  // b listens, a dials, and both run the cookie handshake.
  let listener = b.listen("b").await.expect("bind");
  let dialed = a.connect("b").await.expect("dial");
  let accepted = listener.accept().await.expect("accept");
  let (to_b, to_a) = tokio::join!(
    Session::initiate(dialed, &id_a, &cookie),
    Session::respond(accepted, &id_b, &cookie),
  );
  let (to_b, to_a) = (to_b.expect("a authenticated b"), to_a.expect("b authenticated a"));
  assert_eq!(to_b.peer(), &id_b);

  // Authenticated frames now flow both ways.
  to_b.send(&Frame::Data(b"hello".to_vec())).await.expect("send");
  assert_eq!(to_a.recv().await.expect("recv"), Frame::Data(b"hello".to_vec()));

  // Partitions are scripted, which keeps netsplit tests deterministic.
  net.partition("a", "b");
  assert!(to_b.send(&Frame::Ping(1)).await.is_err());
}
```

For process-level distribution (remote spawn, cast, call, monitors, takeover) see the examples:
`131_two_node_cluster`, `132_distributed_supervisor_failover` and `133_cluster_stream_fanout`.

## Stability

Runtime tier (ADR 0010): public API changes follow the workspace deprecation policy, and the
crate meets the runtime quality bar (`docs/QUALITY_BAR.md`). The wire protocol is versioned
(`PROTOCOL_VERSION`); nodes of different protocol versions refuse to connect.

## Further reading

The book's Part VII covers the Process Mesh end to end:

- [Chapter 36: The Process Mesh](../id_effect/book/src/part7/ch36-00-process-mesh.md)
- [Chapter 38: Distribution](../id_effect/book/src/part7/ch38-00-distribution.md)
- [Chapter 39: Cluster supervision](../id_effect/book/src/part7/ch39-00-cluster-supervision.md)

[`foca`]: https://docs.rs/foca
