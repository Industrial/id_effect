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
