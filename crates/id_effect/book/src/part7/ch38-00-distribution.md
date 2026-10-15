# Nodes, membership, and remote processes

Every node has a `NodeId` (name + boot incarnation). Peers authenticate with
the cookie HMAC handshake over a `Transport` (QUIC preferred, TCP fallback,
`InMemoryTransport` for tests).

Membership is SWIM via `foca` (sans-io). Drive packets and timers yourself
for deterministic partition tests, or pump them over Tokio in production.

Remote spawn names a **registered behavior** plus postcard args (no code
mobility). Everything that waits on the network is an `Effect`: `Send`,
interruptible, and timed on the calling fiber's executor clock.

```rust,ignore
use std::time::Duration;
use id_effect::Effect;

registry.register::<Counter>("counter");

// compose inside a program …
let program: Effect<Vec<u8>, RemoteFault> = mesh
  .remote_spawn("b@host", "counter", args, None, Duration::from_secs(2))
  .flat_map(move |pid| mesh2.remote_call(&pid, msg, Duration::from_secs(2)));
// … or run at the edge
let reply = program.block_on().into_result()?;
```

`remote_cast`, `remote_info`, `remote_monitor`, `remote_demonitor` and `fanout_cast` stay synchronous (at-most-once sends).

For load-aware placement, nodes publish telemetry with `mesh.report_load(..)`
(readable via `mesh.load_reports()`), and `mesh.remote_spawn_placed(..)` (also an
effect) picks a target through the unified
[`place`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_node/src/placement.rs)
scorer instead of naming a node directly.

On `NodeDown`, remote monitors receive `ExitReason::NoConnection`.

**Runnable example:** `cargo run -p id_effect_node --example 131_two_node_cluster`.
