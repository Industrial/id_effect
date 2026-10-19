# Channels and presence

A chat room, a live dashboard, a document several people edit: each is a **topic** that
connections join, that anyone with the right can publish to, and that shows who is there. In
id_effect these are **channels**, a module of `id_effect_rpc` (feature `channels`), not a separate
crate. They run on the process mesh, so a message published on one node reaches the subscribers
on every node.

## The pieces

- **One hub per node.** `Channels::builder().start_on_mesh(&mesh)` spawns a hub process on the
  node's mesh and registers it cluster-wide. Every node registers the hub behavior
  (`channels::register_hub`) before it builds its mesh.
- **Topics are process groups.** A topic's group (the mesh's `ProcessGroups` CRDT) holds the hubs
  of the nodes with subscribers on it. Publishing is one local hand-off per subscriber on this
  node and one cast per *other interested node*, never one per remote subscriber.
- **A join is a subscription.** `Channels::serve_joins` registers a `subscription` procedure
  whose input is `{"topic": …, "meta": …}` and whose items are `ChannelEvent`s, so a browser joins
  over WebSocket or server-sent events like any other subscription.
- **Presence.** Every joined connection is a `Presence` — the principal's key, the connection,
  the node and the metadata it joined with. Subscribers get the member list on join
  (`ChannelEvent::Joined`) and every change after it (`ChannelEvent::Presence` with joins and
  leaves).

```rust
use std::time::{Duration, Instant};
use id_effect::DynEnv;
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, require_role};
use id_effect_node::{BehaviorRegistry, Mesh, MeshFabric, NodeId};
use id_effect_process::{NodeName, ProcessNode};
use id_effect_rpc::channels::{AuthzJoin, ChannelEvent, Channels, JoinSpec, register_hub};

// Two nodes on an in-memory fabric (in production: sessions over QUIC with mutual TLS).
let fabric = MeshFabric::new();
let node = |name: &str| {
  let mut registry = BehaviorRegistry::new();
  register_hub(&mut registry);
  let id = NodeName::new(name);
  Mesh::new(NodeId::fresh(&id), ProcessNode::new(id), registry).with_fabric(fabric.clone())
};
let (a, b) = (node("a@book"), node("b@book"));

// Members of the role "member" may join; the default policy would refuse everyone.
let open = |mesh: &Mesh| {
  Channels::<String>::builder()
    .join_policy(AuthzJoin::new(require_role("member")))
    .start_on_mesh(mesh)
    .unwrap()
};
let (chat_a, chat_b) = (open(&a), open(&b));

let ada = CurrentPrincipal::new(BasicPrincipal::new("ada").with_role("member"));
let bob = CurrentPrincipal::new(BasicPrincipal::new("bob").with_role("member"));
let join = |chat: &Channels<String>, conn: &str, who: CurrentPrincipal| {
  chat
    .join(JoinSpec::new("room:lobby", conn), who)
    .provide_all(DynEnv::new())
    .block_on()
    .into_result()
    .unwrap()
};
let mut on_a = join(&chat_a, "ws-1", ada);
let mut on_b = join(&chat_b, "ws-2", bob);

// The hubs find each other on their heartbeat; then both nodes see both members.
let deadline = Instant::now() + Duration::from_secs(10);
while chat_a.members("room:lobby").len() < 2 || chat_b.nodes("room:lobby").is_empty() {
  assert!(Instant::now() < deadline);
  chat_a.heartbeat();
  chat_b.heartbeat();
  std::thread::sleep(Duration::from_millis(10));
}

// Published on a, delivered on b.
chat_a.publish("room:lobby", Some("ada"), &"hello".to_string()).unwrap();
let heard = loop {
  assert!(Instant::now() < deadline);
  match on_b.try_recv().unwrap() {
    Some(ChannelEvent::Message { body, .. }) => break body,
    Some(_) => {}
    None => std::thread::sleep(Duration::from_millis(10)),
  }
};
assert_eq!(heard, "hello");
# drop(on_a.try_recv());
```

In a service, `chat.serve_joins(&mut group, Procedure::subscription("Chat.join").guard(g))`
serves the join; the WebSocket and SSE transports carry it. Publishing is the application's own
procedure (a mutation or a two-way stream) calling `Channels::publish` behind its own guard.

## Who may join

Every join goes through a `JoinPolicy` — on the first subscribe, and again on every resume,
since a resume is a new call. The default, `DenyAllJoins`, refuses everyone (ADR 0013 R1).
State the rule:

- `join_policy_fn(|join| Decision::allow_if(…))` for a rule on the principal, the topic and the
  join metadata;
- `AuthzJoin::new(policy)` for any `id_effect_authz` policy over the topic (`JoinTarget`), with
  `.with_tenant(…)` when topics belong to tenants (a principal of another tenant is refused before
  the policy runs);
- a type implementing `JoinPolicy` when the decision needs a lookup — its effect runs with the
  call's environment, so it can read a room's members from the database.

`dangerous_allow_all_joins()` admits everyone and is recorded in the posture registry. The join
procedure still needs its own access decision (`.public()` or `.guard(g)`): the guard answers
"may this caller use the procedure at all", the policy "may this principal join this topic".

The principal is the `CurrentPrincipal` in the call's environment (anonymous without one), and
the connection is its `ConnectionId`. A denied anonymous caller gets `{"_tag":
"Unauthenticated"}` (401), an authenticated one `{"_tag": "JoinDenied"}` (403); the policy's
reason stays in the server log.

## Delivery and its limits

- **Ordered per publisher, live, not durable.** A node numbers what it publishes and a receiving
  hub never delivers a node's messages out of order — a duplicate or a late retransmission is
  dropped. A subscriber gets what is published while it is joined. History and catch-up are an
  event-log subscription (the previous chapter) projected on the topic: join the channel for
  what comes next, read the log for what came before.
- **At-most-once by default.** Hub-to-hub casts use the mesh's delivery mode;
  `Mesh::with_delivery(DeliveryConfig::at_least_once())` makes them survive reconnects.
- **Bounded everywhere.** `ChannelsConfig` caps members per topic (10,000), topics per
  connection (32), topics per node (10,000), join metadata (1 KiB) and message size (16 KiB;
  at most 60 KiB, under the 64 KiB mesh frame). A subscriber that falls `subscriber_buffer`
  events behind (256) is dropped with `{"_tag": "Lagged"}` and rejoins; nothing queues without a
  bound.

## When a node goes away

Hubs monitor each other. When a node's sessions drop, the mesh reports it down, the monitor fires
with `noconnection`, and every node removes that node's members at once and pushes the leaves to
its subscribers. A node that is still connected but stops heartbeating (hung, overloaded) loses
its members when they lapse: every `heartbeat_interval` (5 s) each hub sends its members to the
others, and a remote member not refreshed for `presence_ttl` (15 s) is dropped. When the node
comes back, its next heartbeat brings its members back.

`Channels::spawn_heartbeat()` runs the heartbeat on the Tokio runtime. Tests drive it by hand
with a `ManualClock`, so expiry is deterministic.

## The seam

`channels::Backplane` is all a hub needs from the network: interest per topic, fan-out to the
interested nodes, the list of those nodes, a heartbeat and a shutdown. `MeshBackplane` implements
it with process groups and casts; `ChannelsBuilder::start_with` takes another implementation, so
a future transport (mesh RPC) replaces it without touching code written against `Channels`.
