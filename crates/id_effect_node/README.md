# `id_effect_node`

The distribution layer of the [`id_effect`](https://github.com/Industrial/id_effect) Process
Mesh (ADR 0009). `id_effect_process` runs processes on one node; this crate connects nodes into
a cluster and lets processes spawn, message, call, link and monitor each other across it.

## What it provides

| Item | Role |
|------|------|
| `NodeId` | Wire identity: logical `name@host` plus a 128-bit boot incarnation |
| `Cookie`, `handshake` | Mutual HMAC-SHA256 challenge/response; the cookie never crosses the wire |
| `Transport` / `Connection` / `Listener` | Byte-frame pipes. `InMemoryTransport` (deterministic, scripted partitions), `TcpTransport` (length-delimited), `QuicTransport` (feature `quic`) |
| `Session` | An authenticated, framed connection to one peer (`wire::Frame`, postcard, protocol v1) |
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
The handshake authenticates but does not encrypt: use the QUIC transport or a trusted network
for confidentiality.

## Feature flags

| Feature | Enables |
|---------|---------|
| `quic` | `transport::quic::QuicTransport` (quinn + rustls; self-signed certs via rcgen) |
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
  let cookie = Cookie::new("cluster-secret");
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
