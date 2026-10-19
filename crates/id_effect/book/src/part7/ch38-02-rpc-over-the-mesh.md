# RPC over the mesh

The procedures a service serves over HTTP (chapter 33) can be called from other nodes of the
cluster too, over the mesh you already run, with the same contract, the same guards and the same
typed errors. Turn on the `mesh` feature of `id_effect_rpc` (the client half also needs its
default `client` feature).

Mesh calls are always encoded with postcard, so a procedure is served over the mesh only when
its schemas carry a typed codec: `WireSchema::with_serde()` (the `#[rpc::service]` macro emits it
for serde types). Other procedures answer a `Rejected` failure with status 415.

## Wiring a node

The RPC processes are a mesh behavior, so register it before you build the mesh, then make one
`MeshRpc` per node:

```rust,ignore
use id_effect_node::{BehaviorRegistry, Mesh, NodeId};
use id_effect_process::{NodeName, ProcessNode};
use id_effect_rpc::transport::mesh::MeshRpc;

let mut registry = BehaviorRegistry::new();
MeshRpc::register_behavior(&mut registry);
let id = NodeId::fresh(&NodeName::new("relay-1@10.0.0.1"));
let mesh = Mesh::new(id.clone(), ProcessNode::new(NodeName::new(id.name())), registry);
let rpc = MeshRpc::new(id, mesh)?; // inside a tokio runtime (or MeshRpc::on_runtime)
```

**Deliver inbound mesh traffic through the `MeshRpc`**, not straight into the mesh: call
`rpc.handle_session_frame(&session)` where you used to call `mesh.handle_session_frame(&session)`
(or `rpc.ingress(origin, envelope)` in place of `mesh.handle_envelope_from`, and
`rpc.peer_sender(origin)` for in-memory links in tests). The ingress knows which authenticated
session, and so which node, a message came from; the mesh does not tell a process that. It
stamps every message for an RPC process with that node under a key that never leaves this node.
A message that did not come through the ingress carries no valid stamp and is dropped, so a call
never runs on an origin nobody verified. Everything else passes through to the mesh unchanged.

## Serving a service

```rust,ignore
use id_effect_process::{Strategy, SupervisorSpec};

let endpoint = rpc.serve("Chat", chat_group(), env);
let _supervisor = SupervisorSpec::new(Strategy::OneForOne)
  .child(endpoint.child_spec())
  .start(rpc.mesh().process());
```

The endpoint is one mesh process per service, registered under the cluster-wide name
`rpc:Chat` (chapter 39's global registry). Under a supervisor, a crashed endpoint is restarted
and registers the name again. `endpoint.start()` runs it unsupervised.

Every call goes through the same dispatch as HTTP: lookup, then the procedure's guards, then
the handler, under a deadline. The request head the guards see is the **calling node**: its
extensions are `MeshPeer(node_id)` and `RpcPrincipal("mesh:<node name>")`. Nothing of the
calling node's own users crosses the hop, so a procedure guarded for end users refuses mesh
calls (401). Say which procedures other nodes may call with `MeshPeerGuard`:

```rust,ignore
use id_effect_rpc::transport::mesh::MeshPeerGuard;

group.mutation(
  Procedure::mutation("Chat.relay").guard(MeshPeerGuard::nodes(["relay-1@10.0.0.1"])),
  message(), ack(), relay,
);
```

`MeshPeerGuard::any_node()` admits every verified node; both refuse HTTP requests (401).

## Calling it

```rust,ignore
let chat = rpc.client("Chat");
let ack: Effect<Ack, MeshRpcError, ()> = chat.mutation("Chat.relay", &message);
let history = chat.server_stream::<_, Message, ()>("Chat.history", &room); // a futures Stream
let (sink, replies) = chat.bidi::<Message, Message, ()>("Chat.room");      // credits both ways
```

The client resolves `rpc:Chat` on every call, wherever the endpoint runs (the same node
included). Each call is an effect: it carries the calling fiber's span, so the endpoint's
`rpc.request` span (attributes `rpc.service`, `rpc.method`, `rpc.procedure`, `mesh.node`,
`mesh.from`, `mesh.parent`) is its child through the mesh's `TracePropagator` (set it with
`MeshRpc::with_trace_propagator`, as for remote messages in chapter 38).

A `#[rpc::service]` client calls over the mesh through `MeshTransport`, built from a
`MeshRpcClient` and the service's own group, whose typed codecs it needs to write postcard (its
handlers never run on the calling node):

```rust,ignore
use id_effect_rpc::client::mesh::MeshTransport;

let chat = ChatService::client(MeshTransport::new(rpc.client("Chat"), ChatService::group(handlers)));
let ack = chat.relay(message); // every kind, typed errors as over HTTP
```

Failures are typed (`MeshRpcError`):

| Failure | When |
|---|---|
| `Failure(error)` | the procedure's typed error, a reserved failure (`DeadlineExceeded`, `Cancelled`, `ResourceExhausted`, `InvalidInput`, `InternalError`), or `Rejected` with a status (`rejected_status()`: 404, 401/403, 415) |
| `NoEndpoint` | no live process holds the name (or the one the caller knew is gone) |
| `EndpointDown` | the endpoint exited during the call |
| `NodeDown` | the endpoint's node is unreachable |

A call never hangs on a dead endpoint: it monitors the endpoint process, and the monitor fires
when the process exits or when the mesh declares its node down. **Queries** whose endpoint is
gone are re-resolved and retried (three times by default, `with_query_retries`), which makes an
endpoint restart invisible to them. **Mutations are never retried**: the caller sees
`NoEndpoint` / `EndpointDown` / `NodeDown` and decides, because the mutation may have run.

## Limits

All finite by default:

- deadlines: queries, mutations and client streams send the client's timeout (default 30 s) as
  the deadline; the endpoint clamps it to its own cap (`with_max_deadline`, default 30 s);
- open calls: 256 per calling node and 4,096 per endpoint, checked before any guard runs
  (`ResourceExhausted`);
- streams: at most 32 items in flight per direction until the receiver grants more credits; a
  peer that sends beyond its credits gets `ResourceExhausted`; client streams stay within the
  procedure's item and byte caps; every frame is at most 64 KiB;
- a call is dropped, and its handler interrupted, when the caller cancels (drops the call or the
  stream), when the calling node or its reply process goes down, and when the endpoint exits.
