# ADR 0009 — Process Mesh (BEAM-grade processes, supervision trees, and distribution)

## Status

Accepted, amended 2026-10-19 (see [Amendment 1](#amendment-1-2026-10-19-opt-in-at-least-once-remote-send)):
opt-in at-least-once remote send. Amendment 2 (2026-11-27, **Proposed**, see
[Amendment 2](#amendment-2-2026-11-27-revocation-and-certificate-rotation)): revocation and
certificate rotation in the delivery and monitor tables. [ADR 0018](0018-heterogeneous-mesh.md)
(Proposed) amends principle 2, the handshake, membership and the "Byzantine" and "cross-version"
non-goals.

## Context

id_effect 0.4.0 delivered a high-quality **intra-node** concurrency core:

- Structured lifetime: `Scope` (LIFO finalizers, hierarchical close) and
  `CancellationToken` (child-token cascade).
- Restart supervision: `concurrency::Supervisor` + `SupervisorPolicy`
  (Terminate / Restart / RestartWithLimit / Escalate / Ignore) driven by
  `Schedule` on an injected `Clock`.
- A runtime seam: the `Runtime` trait (`spawn_with` takes a `Send` factory
  returning `(Effect, R)`), implemented by `ThreadSleepRuntime` and
  `TokioRuntime`.
- A resource-governance control plane: Compute Fabric
  (telemetry → admission → placement → rebalance) with `WorkProfile`,
  `RebalanceStrategy::ScaleOut`, and the `ClusterResourcePolicy` /
  `FabricJobSpec` stubs reserved by ADR 0007 Phase E.

Everything **between** nodes is missing: process identity, mailboxes,
links/monitors, multi-strategy supervision trees, node membership, a wire
protocol, a cluster registry, and Fabric telemetry that spans hosts.

The workflow charter ADR (`docs/platform/adrs/adr-workflow-charter.md`)
deferred clustering ("use Temporal for multi-region"). **This ADR supersedes
that deferral** for the process/distribution layer. The workflow journal
remains the durability tier; it is complementary, not competing.

## Decision

Build a BEAM/OTP-class process mesh as two new crates plus Fabric extensions.

### New crates

| Crate | Responsibility |
|-------|----------------|
| `id_effect_process` | OTP layer, purely local: typed mailboxes, `Process` behavior (`init`/`handle_call`/`handle_cast`/`handle_info`/`terminate` as Effects), `Pid`/`Addr<M>`, links, monitors, `trap_exit`, exit-signal propagation over `Cause`, supervision trees (`OneForOne`/`OneForAll`/`RestForOne`, restart intensity on `Clock`, shutdown protocol), `DynamicSupervisor`, local name registry |
| `id_effect_node` | Distribution layer: `NodeId`, cookie handshake, `Transport` trait (TCP length-delimited frames; in-memory impl for deterministic tests; QUIC as feature), SWIM-style membership with pluggable discovery, `NodeUp`/`NodeDown` events, remote pids, `BehaviorRegistry` remote spawn, remote links/monitors with `noconnection`, AP CRDT process groups, CP `ClusterSingleton` (PG advisory-lock lease), `DistributedSupervisor`, cluster telemetry aggregation feeding Fabric placement |

### Core design principles

1. **Failure is control flow.** Exit signals, links, monitors, and
   supervision trees are the primary API; `Cause<E>`
   (Fail/Die/Interrupt/Both/Then) carries them.
2. **No code mobility — registered behaviors.** Rust cannot serialize
   closures. Remote spawn names a behavior registered in the binary plus
   Schema-validated init arguments. Every node runs the same image (the
   FLAME assumption).
3. **No preemption — admission.** Fairness comes from Fabric admission
   budgets, interpreter yield points, and per-mailbox processing quotas.
4. **Location transparency with explicit types.** `Pid` is
   location-transparent for `send`; the API distinguishes local and remote
   where semantics differ.
5. **At-most-once messaging, explicitly.** Durability escalates to
   `id_effect_jobs` (outbox/Apalis), never promised by the process layer.
6. **AP by default, CP by choice.** Process-group registry is eventually
   consistent (merge-on-heal); cluster singletons opt into Postgres
   advisory-lock leases.
7. **Each library plays its proficiency.** Tokio: sockets/timers/blocking.
   Axum: control plane. Schema: wire-boundary validation. Fabric: placement.
   id_effect core: semantics.

### Process model

- `Pid = { node: NodeId, local: u64, incarnation: u32 }`. Incarnation
  prevents stale-pid aliasing after restarts.
- Each process is a supervised fiber draining a typed mailbox
  (`coordination::Queue`) with a separate **system channel** for exit
  signals, monitor downs, and stop requests — system messages are never
  blocked behind user messages.
- The process `Scope` owns all its resources; finalizers replace per-process
  GC. Process death always closes the scope.
- `call` = send request + one-shot `Deferred` reply + implicit monitor +
  timeout on `Clock` (BEAM's `GenServer.call` convention).
- **Links** are bidirectional fate-sharing: a non-`normal` exit interrupts
  linked processes unless they set `trap_exit`, in which case the exit
  arrives as `handle_info(Info::Exit { from, cause })`.
- **Monitors** are one-shot, unidirectional: exactly one
  `Down { pid, reason }` delivery (crash, normal, or `noconnection`).

### Supervision trees

```text
ChildSpec { id, start, restart: Permanent | Transient | Temporary,
            shutdown: BrutalKill | Timeout(Duration) | Infinity,
            kind: Worker | Supervisor }
Strategy   { OneForOne | OneForAll | RestForOne }
Intensity  { max_restarts, within }   // measured on the injected Clock
```

- Shutdown protocol: cancel token → wait `shutdown` timeout on `Clock` →
  hard `interrupt()`.
- Intensity exceeded ⇒ the supervisor itself fails, escalating up the tree
  (`Cause::Then` aggregation).
- `concurrency::Supervisor`'s restart-with-`Schedule` remains the leaf
  policy inside a `ChildSpec`; this ADR extends, not replaces, existing
  supervision.
- `DynamicSupervisor` covers homogeneous, template-spawned children.

### Wire protocol v1

- Framing: 4-byte big-endian length prefix, then a `postcard`-encoded
  `Envelope`.
- `Envelope { version: u16, kind, correlation: Option<u64>,
  trace_context: Option<String>, payload: Vec<u8> }`.
- Kinds: `Hello`, `Challenge`, `ChallengeReply`, `Welcome`, `Ping`, `Pong`,
  `Send`, `SysSend`, `SpawnRequest`, `SpawnReply`, `MonitorRequest`,
  `Down`, `LinkRequest`, `Exit`, `GroupDelta`, `LoadReport`, `Bye`.
- Handshake: `Hello(node, version, capabilities)` →
  `Challenge(nonce)` → `ChallengeReply(mac(cookie, nonce))` → `Welcome`.
  Version negotiation is mandatory; mismatched majors refuse to connect.
- Payloads cross the trust boundary as bytes and are decoded through
  `id_effect::schema` (`Unknown` → typed decode with `ParseErrors`):
  parse, don't validate.
- OTel context (`trace_context`) propagates in the envelope.

### Membership

- SWIM-style: periodic probe → suspect → confirm-down, piggybacking
  membership deltas and `LoadReport` telemetry on probe traffic.
- Discovery strategies: static peer list, DNS (Kubernetes headless
  service), gossip seed.
- No full-mesh requirement: membership is gossip; data connections are
  direct, on demand.
- All timing runs on the injected `Clock`, so the in-memory transport +
  `TestClock` make partition scenarios fully deterministic.

### Guarantees (normative)

| Primitive | Guarantee |
|-----------|-----------|
| Local send | Ordered per sender→receiver pair; never lost while receiver alive |
| Remote send | At-most-once; ordered per pair per connection epoch; dropped on disconnect |
| Remote send, reliable (opt-in; [Amendment 1](#amendment-1-2026-10-19-opt-in-at-least-once-remote-send)) | At-least-once to the receiving node, deduplicated per sender incarnation; a retransmitted message can arrive after later ones |
| Monitor | Exactly one `Down` (crash, normal, or `noconnection`) |
| Link | Exit propagation or trapped message; `noconnection` on node loss |
| Group registry (AP) | Eventually consistent; duplicate/gap windows during partition; merge on heal |
| ClusterSingleton (CP) | At most one holder cluster-wide; unavailable without PG |
| Durable work | At-least-once with idempotency keys — `id_effect_jobs` only |

### Netsplit stories (normative)

- **Send:** in-flight messages to a partitioned node are dropped; senders
  needing certainty must monitor.
- **Monitors/links:** partition ⇒ `Down`/`Exit` with reason
  `noconnection` after suspicion timeout. On heal, no resurrection: stale
  pids from the previous connection epoch are dead.
- **Groups:** each side keeps operating on its view; ORSWOT merge on heal
  (add-wins).
- **DistributedSupervisor:** on `NodeDown`, re-places children on surviving
  nodes. Under partition, both sides may run a copy (AP); singletons that
  must not duplicate use `ClusterSingleton` (CP) instead.
- **ClusterSingleton:** the lease holder's side of a partition keeps the
  singleton while its PG session lives; the other side cannot acquire.

### Fabric integration

- `TelemetrySnapshot`s from every node piggyback on membership traffic;
  `compute::cluster` gains a `PlacementScorer` that ranks candidate nodes
  by load, `WorkProfile` affinity, and `PlacementMode`
  (LocalFirst / Spread / Affinity).
- `RebalanceStrategy::ScaleOut` becomes real: saturation triggers remote
  spawns through `id_effect_node`, or durable `id_effect_jobs` enqueue when
  work is marked durable.
- Stream fan-out across nodes is expressed as ordinary stream combinators
  over process groups with admission-bounded concurrency per node.

## Non-goals (v1)

- Hot code loading / rolling cluster upgrades across incompatible majors.
- Cross-version clusters (protocol major must match).
- Exactly-once messaging (use jobs/outbox).
- Preemptive scheduling of native code.
- Byzantine fault tolerance; the cookie boundary assumes a trusted cluster.
- Replacing the workflow journal or Temporal for long-lived durable
  orchestration.

## Consequences

- Two new workspace crates; core `compute/cluster.rs` stubs become the real
  placement API; `FabricJobRunner` in `id_effect_jobs` gets an
  implementation.
- The book gains Part VII (Process Mesh).
- Deterministic distributed testing (in-memory transport + `TestClock`)
  becomes the acceptance bar — a capability BEAM itself lacks.
- Supersedes the "defer clustering to Temporal" stance of the workflow
  charter ADR for the process/distribution layer.

## Phased delivery

| Phase | Scope |
|-------|-------|
| 0 | This ADR, mission, semantics tables |
| 1 | `id_effect_process`: mailboxes, behaviors, links/monitors/trap_exit, supervision trees, DynamicSupervisor, registry |
| 2 | `id_effect_node`: transport (TCP/in-memory; QUIC feature), handshake, SWIM membership, discovery, node monitors |
| 3 | Remote processes: envelope + Schema boundary, BehaviorRegistry, remote spawn/send/call/links/monitors, groups (AP), singleton (CP), OTel propagation |
| 4 | DistributedSupervisor takeover; cluster telemetry aggregation; placement scoring; real ScaleOut; cluster stream fan-out; cross-node backpressure |
| 5 | Axum control plane; chaos suite; soak benchmarks; Book Part VII; examples 130–133 |

## Amendment 1 (2026-10-19, opt-in at-least-once remote send)

Release 0.11.0 (decision D19, be6af99d) adds an opt-in at-least-once mode for remote casts and
bare sends. The "Remote send" row above stays the default and is unchanged; the "Exactly-once
messaging" non-goal stands (durable work still goes through `id_effect_jobs` or an outbox).
Details are in the `id_effect_node::delivery` module docs (`crates/id_effect_node/src/delivery.rs`).

- **Opt-in.** Per message with `Mesh::cast_reliable` / `Mesh::send_reliable` (or
  `SendReliable::send_reliable` on an `Addr`), or per mesh with
  `Mesh::with_delivery(DeliveryConfig::at_least_once())`, which makes every remote cast and send
  reliable. Calls, spawns, monitors and links keep their own semantics.
- **Sender.** Per-peer sequence numbers; a reliable message stays in a retransmit buffer bounded in
  count and bytes until the peer acknowledges it, and is resent on reconnect and on a backoff
  timer. A full buffer drops its oldest message and reports `DeliveryFailure::BufferFull`;
  messages pending when the peer comes back with a new incarnation fail with
  `DeliveryFailure::PeerRestarted`. Failures reach `Mesh::monitor_delivery` monitors.
- **Receiver.** A dedup window per sending node, keyed by (sender incarnation, sequence number),
  delivers each message once; retransmissions are acknowledged again but not delivered again.
  Messages are delivered as they arrive, not put back into sequence order.
- **What is guaranteed.** Delivery to the receiving node's process layer. A message whose target
  no longer exists, or whose target's mailbox is full (dropped and counted in
  `BehaviorRegistry::dropped_full`, as for an at-most-once cast), is still acknowledged. A
  reliable frame from an unknown sender is delivered at-most-once. A sender that restarts forgets
  what it sent, so handlers must be idempotent where a message can arrive twice.
- **Wire.** `ProcessEnvelope::{Reliable, Ack}` and remote trace context on casts, sends and calls
  raise the protocol to version 3; 0.10.x nodes are refused with `VersionMismatch`.

| Primitive | Guarantee |
|-----------|-----------|
| Remote send, reliable (opt-in) | At-least-once to the receiving node while the sender lives; deduplicated per sender incarnation; a retransmitted message can arrive after later ones; a full target mailbox drops the message (counted) and acknowledges it; a full retransmit buffer drops the oldest message (`BufferFull`); pending messages fail on a peer restart (`PeerRestarted`) |

## Amendment 2 (2026-11-27, revocation and certificate rotation)

**Proposed.** Release 0.15.0, leaf `c15-adr0009-amend2`
([#197](https://github.com/Industrial/id_effect/issues/197)), epic #179. It goes with
[ADR 0018](0018-heterogeneous-mesh.md) (attested identities with short-lived certificates) and
[ADR 0019](0019-channel-capabilities.md) (revocable channel capabilities). The default stays
at-most-once. Amendment 1 is unchanged, and the "exactly-once messaging" non-goal stands.

### Why

Under ADR 0018 and 0019, authority can end while both nodes are up and connected: a capability is
revoked, an identity expires unrenewed, or a root or a measurement is removed from the trust
policy. Today the only "authority ended" reason is node loss: `ExitReason::NoConnection`
(`crates/id_effect_process/src/exit.rs:32`), fired by `Mesh::peer_down` and by failed remote
monitors (`crates/id_effect_node/src/mesh.rs:276-300`, `:692-705`). Delivery failures are only
`BufferFull` and `PeerRestarted` (`crates/id_effect_node/src/delivery.rs:147-154`). Reporting
revocation as `noconnection` would be wrong: `noconnection` invites a retry once the node is
reachable again, and a revoked capability must not be retried.

### New reasons (additive)

Both enums are already `#[non_exhaustive]` (`exit.rs:14`, `delivery.rs:147`), so these are
additive:

- `ExitReason::Revoked(RevokeCause)`, on the wire as `ExitReasonWire::Revoked(cause)`, displayed
  as `revoked`. It is abnormal and trappable, like `NoConnection`: a linked process exits unless it
  traps exits.
- `DeliveryFailure::Revoked(RevokeCause)`.
- `RemoteFault::Revoked(RevokeCause)`, and `CapError::Revoked` for RPC (ADR 0019 §6).
- `#[non_exhaustive] enum RevokeCause { CapabilityRevoked, IdentityExpired, RootRemoved,
  MeasurementRemoved, Superseded }`. `Superseded` means the instance was replaced by a crash-only
  hand-off (ADR 0018 §9).

### When revocation happens

| Event | Detected by | Scope |
|---|---|---|
| The exporter, or a gifter with `revocable_by`, revokes a capability (ADR 0019 §8) | the exporter at once; the holder on the `Revoked{cap}` frame or at its next use | that capability and its derivation subtree |
| A peer's certificate reaches `not_after` without a renewal | both sides, at expiry, on the injected clock | the whole session: every capability held over it |
| A `TrustRoots` reload removes the root a peer's chain ends in, or the policy removes its measurement | each node, at the reload | every session whose chain no longer verifies |
| A hand-off cuts over (ADR 0018 §9, step 5) | the supervisor; the predecessor's peers | every session of the predecessor |

### Rotation is not a failure

- **Renewal with the same key.** The host manager issues a new leaf certificate for the same key,
  incarnation and measurement, starting at two thirds of the old lifetime. The node sends
  `Renew{chain, sig}` on each session. The peer verifies the chain and checks that the key,
  `NodeId`, measurement, manifest digest and contract digests are unchanged, then extends the
  session to the new `not_after`. Nothing is observable: no `Down`, no exit signal, no delivery
  failure, and the retransmit buffers stay as they are.
- **A renewal that changes anything else is refused.** The session then ends at the old
  `not_after` with `revoked(IdentityExpired)`.
- **There is no grace period.** The old `not_after` is hard. Renewal starts at two thirds of the
  lifetime, which leaves a third (20 minutes at the default 1 hour) for retries.
- **A key never changes within an incarnation.** The DICE key is per boot. A new key means a new
  boot, which is a new incarnation, and the existing `PeerRestarted` and `noconnection` semantics
  apply.
- **Adding a root changes nothing live.** The operator adds the new root before removing the old
  one (an overlap) when rotating host roots.
- **TLS keeps its certificate.** An open TLS connection keeps the certificate from its handshake.
  `Renew` is an application frame; new connections present the new certificate. The verified
  identity of a session is its latest verified chain.

### Guarantees (normative additions)

| Primitive | On revocation |
|---|---|
| Remote send (at-most-once) | Dropped. If the sender's node already knows the capability is revoked (it got `Revoked`), the send fails at once with `RemoteFault::Revoked`. Otherwise the exporter drops it and counts it. |
| Remote send, reliable (Amendment 1) | Pending messages on the revoked capability, or on a revoked session, fail with `DeliveryFailure::Revoked(cause)` to the `Mesh::monitor_delivery` monitors. They are not retransmitted. Messages already acknowledged stay delivered. A peer that comes back with a new incarnation is still `PeerRestarted`. |
| Call | An in-flight call fails with `CallError::Down(Revoked)` (RPC: `CapError::Revoked`). The exporter interrupts the handler fiber and runs its finalizers, as for a client disconnect (ADR 0014 §2). |
| Monitor | Still exactly one `Down`. It carries reason `revoked(cause)` when the capability it was set up through, or its session, is revoked. In a race between exit, `noconnection` and `revoked`, the first one observed wins and the others are dropped. |
| Link | Exit signal `revoked(cause)` on both sides; trappable. |
| Group registry (AP) | A member whose identity is revoked is removed, as on node down (ORSWOT remove). Revoking a join capability is a leave. |
| ClusterSingleton (CP) | A node whose own identity lapses fences itself: it stops the singleton and releases the lease. |
| Durable work | Unchanged (`id_effect_jobs`). |

### Revocation stories (normative)

- **Effective at the exporter.** A revocation takes effect when the exporter processes it.
  Messages it delivered before stay delivered; frames that arrive after it are refused. Nothing
  else orders a revocation against the holder's in-flight messages.
- **Final.** Unlike `noconnection`, a later reconnect of the same incarnation restores neither the
  capability nor the monitor. A supervisor restart that fails on `Revoked` counts toward the
  restart intensity like any failure, and the restarted child must get a new grant. It does not
  reuse the revoked capability.
- **Partition and revocation.** The exporter enforces at once on its side. Holders on the far side
  learn on heal (from the `Revoked` frame) or when their identity expires. If their suspicion
  timeout passes first, their monitors fire `noconnection`, and because a monitor fires exactly
  once, they never see `revoked`.
- **Latency of root removal.** It is bounded by how fast the reload reaches each node, and
  otherwise by the certificate lifetime (default 1 hour).
- **Self-expiry.** A node whose own certificate expires unrenewed closes all its sessions. Its
  remote monitors fire `revoked(IdentityExpired)` locally.
- **v3 shim peers** (ADR 0018 M1) have no capabilities and never see `Revoked`. For them, revoking
  a session is reported as `noconnection`.

### Wire

Mesh protocol 4 adds the frames `Renew{chain, sig}` and
`Revoked{scope: Cap(CapId) | Session, cause}`, and `ExitReasonWire::Revoked(cause)`.

### Enforcement points

| Rule | Enforcement point (TestExecutor, in-memory transport with scripted partitions) |
|---|---|
| Reliable sends on a revoked capability fail `Revoked` and are not retransmitted | `revoked_cap_fails_pending_reliable_sends_with_revoked` |
| Exactly one `Down`, reason `revoked` | `revoked_cap_fires_monitor_down_revoked_exactly_once`, `revoke_and_peer_loss_race_yields_one_down` |
| Link exits are trappable | `link_exit_revoked_is_trappable` |
| In-flight calls fail and their handlers are interrupted | `inflight_call_fails_revoked_and_handler_interrupted` |
| Expiry, root removal and measurement removal close sessions | `identity_expiry_closes_session_with_revoked`, `trust_roots_reload_revokes_sessions_under_removed_root`, `measurement_removal_revokes_sessions` |
| Renewal with the same key is invisible; any other change is refused | `renewal_same_key_is_invisible`, `renewal_with_changed_measurement_refused` |
| Revoked is final | `revoked_is_not_restored_on_reconnect` |
| At-most-once stays the default | the existing `at_most_once_remains_default` (`crates/id_effect_node/tests/reliable_delivery.rs:330`) |
| New frames are fuzzed | the `mesh_envelope` fuzz target covers `Renew` and `Revoked` |

### Migration

The new variants are additive, and existing `match`es already need a wildcard arm. Code that
treats every abnormal `Down` as "retry" should handle `Revoked` as final. `docs/MIGRATION-0.15.md`
says so. Delivery defaults are unchanged.

### Open questions (defaults taken)

1. **A grace period for late renewals.** Default: none. Renewal starts at two thirds of the
   lifetime.
2. **`revoked` on v3 shim peers.** Default: reported as `noconnection`, since those peers have no
   capabilities.
3. **Fencing of a `ClusterSingleton` holder on self-expiry.** Default: yes. The alternative keeps
   the singleton while its Postgres session lives, as today.
