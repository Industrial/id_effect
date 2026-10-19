# Nodes, membership, and remote processes

Every node has a `NodeId` (name + boot incarnation). Peers authenticate with
the cookie HMAC handshake over a `Transport` (QUIC preferred, TCP fallback,
`InMemoryTransport` for tests).

Membership is SWIM via `foca` (sans-io). Drive packets and timers yourself
for deterministic partition tests, or pump them over Tokio in production.

Remote spawn names a **registered behavior** plus postcard args (no code
mobility). Everything that waits on the network is an `Effect`: `Send`,
interruptible, and timed on the calling fiber's executor clock.

```rust
use std::time::Duration;
use id_effect::Effect;
# use id_effect::{Cause, Never, succeed};
# use id_effect_node::{BehaviorRegistry, Mesh, RemoteFault};
# use id_effect_process::{Next, Process, ProcessCtx};
# struct Counter(u64);
# impl Process for Counter {
#   type Args = u64;
#   type Env = ();
#   type Call = ();
#   type Reply = u64;
#   type Cast = ();
#   type Info = ();
#   type Error = Never;
#   fn init(start: u64, _: ProcessCtx) -> Effect<Self, Never, ()> {
#     succeed(Counter(start))
#   }
#   fn handle_call(self, _: (), _: ProcessCtx) -> Effect<(u64, Next<Self>), Never, ()> {
#     let n = self.0 + 1;
#     succeed((n, Next::Continue(Counter(n))))
#   }
# }
# fn run(
#   mut registry: BehaviorRegistry,
#   mesh: Mesh,
#   args: Vec<u8>,
#   msg: Vec<u8>,
# ) -> Result<(), Cause<RemoteFault>> {
# let mesh2 = mesh.clone();

registry.register::<Counter>("counter");

// compose inside a program …
let program: Effect<Vec<u8>, RemoteFault> = mesh
  .remote_spawn("b@host", "counter", args, None, Duration::from_secs(2))
  .flat_map(move |pid| mesh2.remote_call(&pid, msg, Duration::from_secs(2)));
// … or run at the edge
let reply = program.block_on().into_result()?;
# let _ = reply;
# Ok(())
# }
```

`remote_cast`, `remote_info`, `remote_monitor`, `remote_demonitor` and `fanout_cast` stay synchronous (at-most-once sends).

For load-aware placement, nodes publish telemetry with `mesh.report_load(..)`
(readable via `mesh.load_reports()`), and `mesh.remote_spawn_placed(..)` (also an
effect) picks a target through the unified
[`place`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_node/src/placement.rs)
scorer instead of naming a node directly.

On `NodeDown`, remote monitors receive `ExitReason::NoConnection`.

**Runnable example:** `cargo run -p id_effect_node --example 131_two_node_cluster`.

## Finding peers

A joining node asks a `Discovery` strategy where to announce itself; an empty
answer means it is the first node and forms the cluster. `StaticDiscovery`
holds a fixed list, `DnsDiscovery` resolves one `host:port` to many peers, and
`ChainedDiscovery` tries strategies in order until one answers.

In Kubernetes, feature `kubernetes` adds `KubernetesDiscovery`, which asks the
API server for the ready endpoints of a headless service instead of trusting
DNS. It reads the `discovery.k8s.io/v1` EndpointSlices of the service with its
own small HTTP/1.1 client (no kube-rs) over rustls, and verifies the API server
against the cluster CA; there is no option to skip that check. The
service-account mount (`/var/run/secrets/kubernetes.io/serviceaccount`)
supplies the CA, the namespace, and the bearer token, which is re-read on every
listing because projected tokens rotate.

```rust,ignore
use id_effect_node::discovery::{ChainedDiscovery, Discovery, DnsDiscovery, KubernetesDiscovery};

// API server from KUBERNETES_SERVICE_HOST/PORT; "mesh" is the headless
// service, 4369 the mesh port announced to on every address.
let k8s = KubernetesDiscovery::in_cluster("mesh", 4369)?;
let discovery = ChainedDiscovery::new(vec![
  Box::new(k8s),
  Box::new(DnsDiscovery::new("mesh.team-a.svc.cluster.local:4369")),
]);
let peers = discovery.candidates().await;
```

Endpoints with `ready: false` or `terminating: true` are skipped (an unset
`ready` counts as ready, as the API documents), and the addresses come back
sorted so every node sees the same order. `candidates()` logs a failure and
answers empty, which lets a chain fall through to DNS;
`KubernetesDiscovery::endpoints()` returns the `KubernetesError` instead (`Tls`
for an untrusted API certificate, `Status(401)` or `Status(403)` for a token
or RBAC problem). The service account needs `list` on `endpointslices`:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata: { name: mesh-discovery }
rules:
  - apiGroups: ["discovery.k8s.io"]
    resources: ["endpointslices"]
    verbs: ["list"]
```
