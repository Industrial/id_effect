# ADR 0018 — Heterogeneous mesh

## Status

Proposed (2026-11-27). Release 0.15.0 "Compartments I: Sealed", epic
[#179](https://github.com/Industrial/id_effect/issues/179), leaf `c15-adr0018-hetero-mesh`
([#187](https://github.com/Industrial/id_effect/issues/187)). The owner moves it to Accepted. Until
then every 0.15 leaf that cites it cites it as Proposed. It amends [ADR 0009](0009-process-mesh.md)
(principle 2, the handshake, membership and two non-goals). Read it together with
[ADR 0019](0019-channel-capabilities.md) (channel capabilities) and
[ADR 0009 Amendment 2](0009-process-mesh.md#amendment-2-2026-11-27-revocation-and-certificate-rotation)
(revocation in the delivery and monitor tables).

Owner decisions already taken, and not re-asked here:

- **No live migration** (2026-11-27, comment on #187). Compartments are crash-only, and state is
  handed over by contract, fleet-wide. §9 records this as a non-goal and defines the hand-off
  contract.
- **Identity is pure DICE chains rooted in each host's TPM, with no fleet CA** (2026-11-27,
  nixos-dotfiles#419, "Owner decisions, round 2", item 2). Each host's attestation root is pinned
  in the operator's trust registry (in the fleet, `common/fleet.nix`, reviewed like
  `operatorKey`). Revocation means removing the root and letting the short certificate lifetimes
  run out.
- **VPS is out of scope** (same comment, item 3; nixos-dotfiles#429 closed as not planned). This
  ADR has no confidential-VM tiers, no remote attestation verifier and no VPS admission.

The design comes from the compartments research (nixos-dotfiles `docs/research/compartments/`,
reports 03 "prior art and policy" and 04 "build, attestation and mesh", §4). The open questions
are at the end, with the defaults this ADR takes.

## Context

ADR 0009 built a BEAM-class mesh for **one trusted cluster running one image**. The 0.15
programme puts every program in its own hardware-isolated compartment (a microVM booted from a
per-closure image), and each compartment is a mesh node. Those nodes are different binaries, built
independently, and any one of them may be compromised. Every assumption below holds for the code
at `origin/main` (v0.13.0), and every one fails for compartments. Paths are under
`crates/id_effect_node/src/` unless they say otherwise.

| ADR 0009 assumption | Where the code relies on it | Why it fails for compartments |
|---|---|---|
| **Same binary (FLAME).** Principle 2: "Every node runs the same image." | `registry.rs:1` ("BehaviorRegistry for remote spawn (FLAME model)"). `BehaviorRegistry::spawn` (`registry.rs:84-95`) resolves a behaviour name in the local binary. `Mesh::dispatch` answers every `ProcessEnvelope::Spawn` from any authenticated peer (`mesh.rs:841-857`). `DistributedSupervisor` re-spawns a lost node's children by behaviour name on any survivor (`dist_supervisor.rs:1-8`, `:132-143`). | The peer does not contain your behaviours. A spawn request from a leaf is an attack, not a feature. |
| **Membership is a shared cookie.** | `Cookie` (`handshake.rs:66`) MACs the transcript (`handshake.rs:149-170`). `Session::initiate` and `respond` take it (`session.rs:38-54`). QUIC says "the cookie gives cluster membership" (`transport/quic.rs:53-56`). | Every compartment would hold the one secret that admits any identity. One compromised leaf can then join as anything. |
| **Reachability by pid.** `Pid` is location-transparent for `send`. | `Mesh::remote_cast` and `remote_info` take any `&Pid` (`mesh.rs:474-496`). Inbound `Cast`, `Info` and `Call` go to whatever pid the frame names, whoever sent it (`mesh.rs:759-790`). `Pid` is `{node, id, incarnation: u32}` (`crates/id_effect_process/src/pid.rs:46-50`), so pids can be guessed. | Knowing a pid is ambient authority to message it. That is the confused-deputy shape (report 03 §3.2, lesson 3). |
| **One protocol version per cluster.** | `PROTOCOL_VERSION = 3` (`wire.rs:27`). Both handshake sides refuse any other value (`handshake.rs:331-345`, `:404-415`). ADR 0014 §5 keeps mixed-version mesh interop a non-goal. | Independently built images drift apart. One version for the whole fleet means a flag day at every rebuild. |
| **Gossip among equals.** | SWIM via foca (`membership.rs:1`). Every member learns every member. | A leaf should not learn the topology, and must not be able to inject membership. |
| **Principals are node names.** | `MeshPeerGuard::nodes` admits by `name@host` string (`crates/id_effect_rpc/src/transport/mesh.rs:296-321`). RPC clients find endpoints by global name (`crates/id_effect_rpc/src/client/mesh.rs:289`, `whereis(endpoint_name(..))`; `global_registry.rs:288`). | A name says nothing about what code runs there or what it may do. |
| **Byzantine faults are a non-goal.** | ADR 0009 non-goals; `THREAT-MODEL.md:68-70`, B4 last row (`:124`), "Trusted cluster" (`:344-349`). | Compromised leaves are the main threat of the programme (§8). |

These parts carry over unchanged:

- the `Transport`/`Connection` seam with TLS channel binding (`transport/mod.rs:129-176`,
  `channel_binding` at `:152`), mutual TLS on QUIC and TCP (`transport/quic.rs:25-50`), and the
  certificate code in `id_effect_platform::tls` (`transport/certs.rs:1-10`, ADR 0014 §5);
- `NodeId`, the name plus a 128-bit boot incarnation (`id.rs:14-37`), which already makes every
  restart a new identity;
- the pre-authentication bounds (`handshake.rs:40-48`) and Schema decoding of every payload;
- the RPC mesh ingress, which stamps each message with the verified origin under a node-local key
  (`crates/id_effect_rpc/src/transport/mesh.rs:17-28`, `ingress` at `:607`, `stamp_envelope` at
  `:685`), with per-peer call limits, deadlines, credits and heartbeats (`:36-54`);
- the delivery semantics of ADR 0009 and Amendment 1 (`delivery.rs`), extended by Amendment 2.

## Decision

### 1. What a node is

A mesh node is either a **compartment** (one program in one microVM, booted from one measured
image) or a **host manager** (the compartment manager on a physical host, the root
`CompartmentSupervisor` of [#195](https://github.com/Industrial/id_effect/issues/195)). Nodes may
run different binaries. The same-image cluster of ADR 0009 is the special case where every node
offers the same contracts. It keeps working (§10), but nothing in the mesh assumes it.

ADR 0009 principle 2 becomes: **no code mobility, and no ambient spawn.** A node spawns only its
own processes. To start a program elsewhere, a node asks that host's manager to boot an image,
named by its derivation and measurement, through a `Compartments` contract that the manager
offers (#195). `ProcessEnvelope::Spawn` (`envelope.rs:291`) is refused unless the receiving node
offers that behaviour as a spawn capability in its manifest (ADR 0019 §2). By default it offers
none.

### 2. Reachability by capability, not by pid

A message reaches a process only through a **channel capability** that the sender holds (ADR
0019). A `Pid` that arrives on the wire is inert data. It names a process; it does not authorise a
message to it. Concretely:

- every inbound user frame (`Cast`, `Info`, `Call`, `Monitor`, `Link`, RPC frames) names a
  capability in the session's import table. A frame that names a bare pid is refused with
  `RemoteFault::NoCapability` and counted.
- `Mesh::remote_cast(&Pid, ..)` and the other send-by-pid functions give way to sends on a
  `ProcessCap<M>`, or to an RPC client bound to a `ChannelCap<S>` (ADR 0019 §5).
- the cluster-wide name registry (`global_registry.rs`) stays, for host managers among
  themselves. A leaf gets its peers from its **bootstrap capabilities** (the manifest), never by
  looking up a name.

### 3. Identity: DICE chains rooted in each host's TPM

Every node has an **attested identity**: an Ed25519 key, plus a short X.509 chain that says what
the node is.

```text
host TPM ── endorses ──▶ host DICE root  (public key pinned in the operator's TrustRoots)
                           │ CDI_host = measured-boot secret, unsealed only on matching PCRs
                           ▼
                         host manager cert   (role = manager, host = H)
                           │ CDI_attest(c) = KDF(CDI_host, measurement(c) ‖ manifest digest)
                           ▼
                         compartment leaf cert  (per boot; key derived from CDI_attest(c))
```

- **No fleet CA.** A chain is valid when it ends in a root in the verifier's `TrustRoots`. The
  library takes `TrustRoots` as a capability (a Layer). Where the roots come from (in the fleet,
  `common/fleet.nix`, reviewed like `operatorKey`) is the operator's business. id_effect never
  reads fleet configuration.
- **The leaf certificate carries**, in extensions, the node's `NodeId` (name and incarnation), its
  role (`manager` or `compartment`), its **trust tier** (§6), the **measurement** (the image's UKI
  hash, which commits to the verity root of the closure), the **manifest digest**, and the
  **contract digests** of every contract it offers and uses
  ([#189](https://github.com/Industrial/id_effect/issues/189), ADR 0019 §7). The measurement uses
  the TCG `DiceTcbInfo` extension (OID 2.23.133.5.4.1). The manifest and contract digests use an
  id_effect extension that #191 defines.
- **Host binding.** A compartment chain must pass through the manager certificate of the host the
  compartment claims to run on. A verifier accepts the chain only when the system description
  places that compartment on that host (§6, ADR 0019 §1). A compromised host can therefore mint
  identities only for its own compartments.
- **Short lifetimes.** Leaf certificates live for `identity.lifetime` (default 1 hour). The host
  manager renews them in session at two thirds of their life (Amendment 2). The DICE key is per
  boot, so a restart brings a new key, a new incarnation and a new certificate.
- **Revocation is by removal.** Remove a host root from `TrustRoots`, or a measurement from the
  allowed set. A `TrustRoots` reload closes every session whose chain no longer verifies, with
  reason `revoked` (Amendment 2). A node that misses the reload stops trusting the removed root
  when the certificates under it expire. There is no CRL and no OCSP.
- **What id_effect ships in 0.15** is the certificate format, the verifier (`IdentityVerifier`, a
  Layer over `TrustRoots` and a measurement policy), a deterministic fake issuer for tests, and the
  handshake. TPM unsealing and the KDF chain live in the compartment manager and in
  `id_effect_attest` ([#200](https://github.com/Industrial/id_effect/issues/200), 0.16). Both wrap
  vendor crates and ship no crypto of their own.

### 4. Handshake v4: attested, no cookie

[#191](https://github.com/Industrial/id_effect/issues/191) implements it; this ADR fixes its shape.

1. **TLS 1.3 with mutual authentication** on every transport: QUIC, TCP with `tls-tcp`, and vsock
   with [#190](https://github.com/Industrial/id_effect/issues/190). The leaf certificate is the TLS
   certificate. A custom rustls verifier checks the chain against `TrustRoots`, the validity
   period and the measurement policy, in both directions. The identity check replaces the
   `"cluster"` server-name check (`transport/quic.rs:59`, `DEFAULT_SERVER_NAME` at `:135`).
2. **The mesh handshake** runs over the TLS channel:
   `Hello{protocol range, nonce, offered contracts}` →
   `HelloReply{chosen protocol, nonce, offered contracts, sig}` → `Confirm{sig}`. Each `sig` is the
   node key's Ed25519 signature over the whole transcript, the role and the TLS exporter secret
   (`CHANNEL_BINDING_LABEL`, `handshake.rs:52`). The signatures bind the negotiated versions and
   contracts to the authenticated keys, so a downgrade is detected. They also let the in-memory
   transport run the same handshake in tests.
3. **The pre-authentication bounds stay.** The 5 s timeout is unchanged. The pre-auth frame cap
   rises from 4 KiB to 16 KiB so that a certificate chain fits, and it is still checked before
   buffering (ADR 0013 R4).
4. **The cookie leaves the default path.** Handshake v3 survives one minor (L9) as
   `Cookie::insecure_shared_cookie_membership()`, which records itself in the posture registry.
   The prod profile refuses it (E14).

### 5. Protocol and contract versions, per link

- **Protocol, per link.** Each side advertises the range of mesh protocol versions it speaks. The
  link uses the highest version both speak. If the ranges are disjoint it is refused with
  `HandshakeError::VersionMismatch { ours: range, theirs: range }`. A release speaks its own
  version and the one before it, so a fleet can rebuild one image at a time. Protocol 4 is the
  first version under this rule; only the v3 shim speaks 3.
- **Contracts, per channel.** A channel opens for one contract, `(service name, major)`. Each side
  states `{name, major, revision, digest}` (ADR 0019 §7). The channel opens only if both of these
  hold:
  - the peer's certificate lists that digest, so a peer cannot claim a contract it was not built
    with;
  - the pair of digests is in the **wiring** of the system description. The builder checked every
    wired pair with `rpc check` (#189, [#193](https://github.com/Industrial/id_effect/issues/193)).

  At run time this is an allowlist lookup. The runtime never runs a compatibility check on input
  that was not measured. A mismatch is the typed `ChannelError::ContractMismatch`, never a decode
  failure halfway through a call.

### 6. Trust tiers

```rust
#[non_exhaustive]
pub enum TrustTier { Dev, Software, Attested }   // ordered: Dev < Software < Attested
```

| Tier | Root | Use | Prod profile |
|---|---|---|---|
| `Attested` | a TPM-endorsed host DICE root in `TrustRoots`, with the measurement in the allowed set | every compartment and host manager in the fleet | allowed |
| `Software` | a pinned root whose key is held in software (a file, a KMS), with no TPM endorsement | meshes that are not compartments: the `cluster` template, Kubernetes deployments moving off the cookie | allowed, but a channel may require `Attested` |
| `Dev` | the fake issuer, or a self-signed leaf | tests and `id-effect dev` | refused (`insecure_dev_identity`, recorded) |

- The issuer sets the tier, and the certificate carries it. Each `TrustRoots` entry caps the tier
  that chains under it may claim, so a software root cannot issue `Attested` leaves.
- Every bootstrap and live capability carries a `min_tier` caveat (ADR 0019 §3). A holder below it
  is refused at the handshake, at a handoff and at every call.
- The enum is `#[non_exhaustive]`, so a future ADR can add confidential-VM tiers if VPS comes
  back. This ADR adds none.

### 7. Membership and topology

- **Membership means holding a verifiable identity and being wired.** There is no shared secret. A
  verified peer that is in no wired pair for this node gets a session with an empty import table:
  it can do nothing but close.
- **Hub and spoke per host.** A compartment holds sessions to its host manager and to the peers
  its bootstrap capabilities name, and to no one else. SWIM gossip (`membership.rs`) runs **among
  host managers only** (role `manager`, tier `Attested` or `Software`). A leaf learns `NodeUp` and
  `NodeDown` for its wired peers from its manager. Gossip packets from a leaf are dropped and
  counted.
- **`DistributedSupervisor`** (`dist_supervisor.rs`) keeps working in same-image meshes. Under
  compartments, its takeover becomes the `CompartmentSupervisor`'s restart of an image (#195), not
  a behaviour spawn on a survivor.
- `ClusterSingleton` (the Postgres lease) and AP process groups keep their semantics. A group join
  is a capability like any other (ADR 0019).

### 8. Threat model: compromised leaves

This narrows ADR 0009's "Byzantine fault tolerance" non-goal. **A compromised leaf is in the
threat model.** A compromised host manager or host kernel is in the TCB of the compartments on
that host, and of no others. Consensus among malicious managers stays a non-goal.

Assets: the authority each capability grants; secrets released to identities (the vault, #202);
the integrity of results passed between compartments; the availability of shared services.

Actors: **A1** a compromised leaf (arbitrary code inside one compartment, holding its key and its
capabilities); **A2** an on-path network attacker; **A3** a compromised host (manager, kernel or
VMM) on one host; **A4** an operator mistake in the system description or `TrustRoots`.

| # | STRIDE | Threat | Mitigation | Enforcement (§ "Enforcement points") |
|---|---|---|---|---|
| T1 | E | A1 messages a process it was not granted, by guessing or replaying a pid. | No ambient pids (§2). A frame must name a capability in the session's import table. | E1, E2 |
| T2 | E | A1 spawns code on a peer (ADR 0009 B4 last row). | No ambient spawn (§1). Spawn is refused unless offered as a capability. | E3 |
| T3 | S | A1 joins as another node, or as a manager. | No cookie (§4). The chain must verify to a pinned root, the role and host must match the system description, and the key is bound by the TLS handshake and the transcript signature. | E4, E5, E6 |
| T4 | S | A1 claims a contract or a measurement it was not built with. | The issuer takes the digests from the measured image and puts them in the certificate. A channel opens only on a wired digest pair (§5). | E7, E8 |
| T5 | T | A2 downgrades the protocol or a contract revision. | The transcript signature covers both advertised ranges and both offers. | E9 |
| T6 | I | A1 learns the topology or injects membership. | Gossip runs among managers only. A leaf sees only its wired peers. | E10 |
| T7 | D | A1 floods a shared service. | Per-peer call limits, credits and deadlines (the RPC mesh has them today). Per-capability rate caveats and bounded import and export tables (ADR 0019). Pre-auth bounds (§4). | E11, ADR 0019 |
| T8 | T | A1 sends hostile payloads. | Schema decode at the boundary, as today. The new decoders (handshake v4, certificate extensions, capability descriptors) get fuzz targets. | E12 |
| T9 | E | A1 leaks its own key to an outside attacker. | The key is per boot and the certificate is short-lived. It authorises only what that compartment was granted, from the host it runs on. Revoke by measurement or by root. | E5, Amendment 2 |
| T10 | S, E | A3 mints identities. | Accepted inside A3's own host, which is the TCB of its compartments. Host binding (§3) stops it from minting identities for compartments placed elsewhere. Removing the host root revokes all of them. | E6 |
| T11 | E | A4 wires too much, or pins a wrong root. | Out of the library's reach. The controls are the system description checker (#193) and the review of `TrustRoots`. The prod posture refuses `Dev` identities and the cookie shim. | E13, E14 |
| T12 | T | Warm state handed to a new instance is forged or replayed (§9). | The hand-off channel is a single-use capability between the two attested instances. The state is Schema-decoded, and its generation must be greater than the last one committed. | E15–E17 |

Out of scope:

- physical attacks on a host, and TPM extraction;
- micro-architectural side channels between compartments (a per-pair decision in the system
  description, report 03 §3.2, lesson 6);
- VPS and confidential VMs;
- information-flow labels (later);
- denial of service by a host against its own compartments.

### 9. No live migration: crash-only with a hand-off contract

**Live migration is a non-goal** (owner decision). No VM memory snapshot, restore or transfer is
offered between hosts. A compartment is crash-only: it can be killed at any instant, and its
replacement starts from durable state. To move a compartment, the supervisor stops one instance
and starts another, possibly on another host and possibly from a newer image.

**Durable state is the source of truth.** Anything that must survive a crash is on the
compartment's volume, in the workflow journal or in `id_effect_jobs` before it is acknowledged.
The hand-off below moves **warm** state: caches, in-memory indexes, and open sessions that a
client can resume. It is an optimisation. If it fails, the new instance starts cold and is still
correct.

The hand-off contract is an ordinary `#[rpc::service]` contract, versioned and digested like any
other (ADR 0014 §9, ADR 0019 §7):

```rust
#[rpc::service(version = 1)]
pub trait StateHandoff {
  /// The outgoing instance streams its warm state; the incoming instance answers once it is stored.
  #[guard(HandoffGrant)]
  #[client_stream(max_items = 65_536, max_bytes = 1_073_741_824)]
  fn transfer(manifest: HandoffManifest, chunks: Stream<StateChunk>)
    -> Effect<HandoffReceipt, HandoffError, R>;
}

pub struct HandoffManifest {
  pub state_schema: ContractDigest,  // the digest of the state type's own schema
  pub generation: u64,               // strictly greater than the last committed generation
  pub from: NodeId,                  // outgoing incarnation
  pub total_bytes: u64,
  pub sha256: [u8; 32],              // over the concatenated chunk payloads
}
pub struct StateChunk { pub seq: u32, pub bytes: Bytes }
pub struct HandoffReceipt { pub generation: u64 }
pub enum HandoffError { SchemaNotAccepted, Stale { last: u64 }, Corrupt, TooLarge, Deadline }
```

The `CompartmentSupervisor` (#195) drives the protocol, on the injected clock:

1. **Start the successor.** Boot the new instance I (new key, new incarnation, possibly a new
   image). I verifies its durable state and **does not serve** its offered channels yet.
2. **Drain the predecessor.** Send `Drain{deadline}` to the outgoing instance O on its system
   channel. O stops accepting new calls, which get `Unavailable { retry_after }`; callers retry on
   I with their idempotency keys (ADR 0014 §11). O finishes or cancels its in-flight work by the
   deadline and flushes its durable state.
3. **Grant the hand-off.** The supervisor hands O a capability to I's `StateHandoff` through a
   three-party handoff (ADR 0019 §4). It is single use, its TTL is the drain deadline, its
   `min_tier` is O's tier, and only O can redeem it.
4. **Transfer.** O streams its warm state. I checks that its manifest accepts `state_schema`, that
   `generation` is newer than the last committed one, and the size and digest. The supervisor
   holds the last committed generation now; the vault's monotonic counters hold it later (#202).
   Then I stores the state and answers `HandoffReceipt`.
5. **Cut over.** O exits `Normal`. Its certificate is not renewed, and its sessions close with
   reason `revoked` (Amendment 2). The supervisor re-points the bootstrap capabilities of O's
   clients to I (capabilities name a role and a contract, not a pid; ADR 0019 §1), and I starts
   serving.

A failure at any step takes the crash path. If O dies, the drain deadline passes, or the transfer
fails, the supervisor revokes the hand-off capability, records
`HandoffOutcome::Abandoned(reason)`, and I starts from durable state. Two instances never serve
the same offered channels at once: O stops serving at step 2, and I serves only after step 5 or
after abandonment. Messages in flight to O follow the delivery tables: at-most-once sends are
dropped, reliable sends fail with `PeerRestarted` or `Revoked`, and calls fail and are retried by
the caller.

### 10. Same-image meshes

Existing ADR 0009 users (the `cluster` and `worker` templates, Kubernetes deployments) get the same
rules with less ceremony:

- identity from a `Software` root (an operator-held key that issues node leaves), or `Dev` in
  tests;
- one manifest for the whole cluster that offers every registered behaviour to every peer with the
  same image digest. Remote spawn, `DistributedSupervisor` and process groups work as before, now
  as capabilities that manifest grants.
- `Mesh::dangerous_ambient_pids()` for one minor (L9). It restores pid reachability among peers
  with the same image digest only. It records itself, and the prod posture refuses it unless the
  operator accepts it.

### 11. VPS: out of scope

This ADR has no confidential-VM tiers, no SEV-SNP or TDX evidence, no attestation verifier
compartment, no elastic VPS pool and no k-of-n result acceptance. `TrustTier` is
`#[non_exhaustive]` so that a later ADR can add them. Placement constraints by tier
([#201](https://github.com/Industrial/id_effect/issues/201)) follow in 0.16, for local hosts only.

## Enforcement points

These follow the ADR 0013 style: each rule holds while its enforcement point is green. Each test
id starts with the ADR 0013 rule it enforces (R1 deny by default, R4 bounded input, R8
authenticated transports, R10 recorded opt-outs). The owning leaf's acceptance criteria name the
same ids.

| # | Rule | ADR 0013 | Enforcement point (mechanical) | Leaf |
|---|---|---|---|---|
| E1 | A frame that names a bare pid is refused. | R1 | `r1_cast_to_ungranted_pid_refused`, `r1_call_to_ungranted_pid_refused`, `r1_monitor_without_capability_refused` | #188 |
| E2 | No public API sends to a remote node by pid. | R1 | compile-fail `r1_remote_cast_by_pid` (`tests/security_compile_fail`) | #188 |
| E3 | Spawn is refused unless offered as a capability. | R1 | `r1_spawn_of_unoffered_behavior_refused`; `r1_spawn_offered_in_manifest_succeeds` | #187, #188 |
| E4 | v4 has no cookie. A peer whose chain does not reach a pinned root is refused before any frame. | R8 | `r8_peer_with_unpinned_root_refused`, `r8_self_signed_leaf_refused_outside_dev`, `r8_v4_handshake_takes_no_cookie` | #191 |
| E5 | An expired leaf, or a measurement outside the policy, is refused. | R8 | `r8_expired_identity_refused`, `r8_measurement_not_in_policy_refused` | #191 |
| E6 | A compartment chain must pass through the manager of the host the system description places it on. | R8 | `r8_identity_from_wrong_host_root_refused`, `r8_leaf_claiming_manager_role_refused` | #191 |
| E7 | A channel needs the contract digest in the peer's certificate. | R1 | `r1_contract_not_in_peer_certificate_refused` | #189, #191 |
| E8 | A channel needs a wired digest pair. Majors never mix. | R1 | `r1_unwired_contract_pair_refused`, `r1_contract_major_mismatch_refuses_channel` | #189 |
| E9 | Each link negotiates its protocol. A downgrade is detected. | R8 | `r8_link_negotiates_highest_common_protocol`, `r8_disjoint_protocol_ranges_refused`, `r8_protocol_downgrade_detected` | #191 |
| E10 | Leaves do not gossip. | R1 | `r1_leaf_membership_packets_dropped` | #187 |
| E11 | Pre-auth frames stay bounded (16 KiB, 5 s). | R4 | `r4_v4_oversized_pre_auth_frame_rejected`, `r4_v4_handshake_times_out` | #191 |
| E12 | New decoders are fuzzed. | R4 | fuzz targets `mesh_handshake_v4` and `identity_certificate_extensions` in ClusterFuzzLite, no open crash | #191 |
| E13 | `Dev` identities, the cookie shim and ambient pids are recorded opt-outs. | R10 | `insecure_dev_identity_is_recorded`, `insecure_shared_cookie_membership_is_recorded`, `dangerous_ambient_pids_is_recorded`; `scripts/check-dangerous.sh` | #187, #191 |
| E14 | The prod profile refuses them. | R10 | `posture_prod_refuses_dev_identity`, `posture_prod_refuses_cookie_membership`; posture probes "mesh membership attested" and "mesh reachability by capability" in `id_effect_kit::posture`, beside `MESH_ENCRYPTED` (`crates/id_effect_kit/src/posture.rs:84`) | #187 |
| E15 | Hand-off: never two writers. | R1 | `handoff_successor_serves_only_after_commit_or_abandon`, `handoff_predecessor_refuses_new_calls_after_drain` (TestExecutor, fake hypervisor) | #195 |
| E16 | Hand-off: the capability is single use, and only the predecessor can redeem it. | R1 | `r1_handoff_capability_redeemed_twice_refused`, `r1_handoff_capability_redeemed_by_other_identity_refused` | #188, #195 |
| E17 | Hand-off: stale or corrupt state is refused, and a crash falls back to durable state. | R4 | `handoff_stale_generation_refused`, `handoff_digest_mismatch_refused`, `crash_during_handoff_starts_from_durable_state` | #195 |
| E18 | Peer input cannot build a `TrustTier`. | R8 | compile-fail `r8_trust_tier_from_wire` (no `Deserialize`; only the verifier constructs a `VerifiedPeer`) | #191 |

#187 rewrites `THREAT-MODEL.md` B4 from §8. Once E1–E14 are green, the "Trusted cluster (ADR
0009)" accepted risk (`THREAT-MODEL.md:344-349`) is retired.

## Migration from ADR 0009

| Step | Release | Change | Shim (L9) |
|---|---|---|---|
| M1 | 0.15 | Handshake v4 and attested identities land beside v3. New meshes default to v4. | v3 behind `Cookie::insecure_shared_cookie_membership()`, recorded |
| M2 | 0.15 | Capability reachability (ADR 0019) is on by default. | `Mesh::dangerous_ambient_pids()`, limited to the same image digest, recorded |
| M3 | 0.15 | Remote spawn needs an offered spawn capability. The same-image manifest (§10) offers every registered behaviour. | none needed for same-image meshes |
| M4 | 0.15 | `MeshPeerGuard::nodes(names)` is deprecated in favour of capability guards (ADR 0019 §6). | `#[deprecated]` for one minor |
| M5 | 0.15 | Gossip runs among managers only, when roles are present. Without roles (same-image), every node is a manager. | — |
| M6 | 0.16 | v3, the cookie and ambient pids are removed. | — |

- `docs/MIGRATION-0.15.md` gets one entry per step, with code before and after. In 0.15 the
  `cluster` and `worker` templates move to a `Software` root and the same-image manifest, and their
  prod posture is clean without opt-outs.
- A 0.14 node can join a 0.15 v4 mesh only through the v3 shim. A 0.15 node speaks protocols 3
  (the shim) and 4, and a 0.16 node speaks 4 and 5. Mixed-version meshes are supported within that
  window. For the mesh protocol, this replaces ADR 0009's "cross-version clusters" non-goal and
  ADR 0014 §5's "mixed-version mesh interop" non-goal.
- The book's Part VII (Process Mesh) gains a chapter on identities, capabilities and the hand-off.

## Consequences

Positive:

- A compromised leaf can use only what it was granted, from the host it was placed on. The mesh
  stops depending on every member being honest.
- Identities are derived, not distributed: there is no shared secret to leak and no CA to
  compromise. The fleet's trust registry stays the one place where trust is configured.
- Images can be rebuilt and rolled out one at a time, because each link negotiates its protocol
  version and each channel its contract version.
- Crash-only moves keep one code path (start from durable state). The hand-off is an optimisation,
  and killing it tests that path.

Negative:

- Every mesh user gets semver breaks: the handshake, the send API and remote spawn. The shims cost
  one minor of double code paths.
- Certificate chains add a few KiB and one signature per side to each connection, and short
  lifetimes add renewal traffic. The 0.15 bench gate measures connect latency, v3 against v4.
- Without a CA, revoking a host is as fast as the `TrustRoots` reload reaches each node, and
  otherwise as slow as the certificate lifetime (default 1 h).
- Same-image users who never needed any of this get a `Software` root and a manifest to manage.

## Alternatives considered

- **Keep the cookie and add per-node certificates.** Rejected: the cookie would still admit any
  holder as any identity, so a compromised leaf could still join as anything.
- **A fleet CA in an attestation compartment.** Rejected by the owner. With VPS gone, its main
  benefit (admitting VPS nodes) is gone too, and it would be one high-value target in the TCB.
- **A vTPM (swtpm) per compartment.** Rejected as the default: it puts a large C process per VM in
  the host TCB, where DICE needs a few hundred lines of KDF and signing. It stays an option for
  foreign software that needs TPM APIs, outside id_effect.
- **Live migration (cloud-hypervisor snapshot and restore across hosts).** Rejected by the owner.
  It would also keep identity keys in RAM across hosts, which defeats host binding, and the
  cloud-hypervisor migration channel is unencrypted.
- **Purely static wiring, with no runtime capability passing.** Rejected: it cannot express "the
  file picker hands the editor one file", or the hand-off itself. ADR 0019 keeps static roots and
  adds attenuated passing.
- **Noise XX instead of TLS for the session.** Deferred: one crypto stack (rustls) is less to audit
  now. A Noise transport is a possible later cut to the TCB.
- **Keep gossip among all nodes.** Rejected: leaves would learn the topology and could inject
  membership.

## Open questions (defaults taken)

1. **Leaf certificate lifetime.** Default: 1 hour, renewed at two thirds. Shorter makes root
   removal take effect faster; longer costs less renewal traffic.
2. **Certificate encoding: X.509 or CBOR (COSE, which Open DICE allows).** Default: X.509, so that
   rustls verifies the chain during the TLS handshake. CBOR would be smaller but needs a second
   verifier.
3. **The `Software` tier.** Default: it exists, so that meshes that are not compartments can drop
   the cookie in 0.15. The alternative, DICE only for every mesh, strands meshes without a TPM.
4. **Protocol support window.** Default: the current version and the one before it. A wider window
   eases rolling upgrades but keeps more code.
5. **Pre-auth frame cap for v4.** Default: 16 KiB, which fits a chain of two or three
   certificates. A larger chain is a configuration error.
6. **Where the last committed hand-off generation lives in 0.15.** Default: in the
   `CompartmentSupervisor`'s durable state on the host. It moves to the vault's monotonic counters
   with #202 in 0.16.
7. **The shim window.** Default: one minor (removed in 0.16), as L9 requires.

## References

- Epic #179. Leaves: #187 (this ADR), #188 (ADR 0019), #189 (contract identity), #190 (vsock),
  #191 (handshake v4), #192 (capability manifest), #193 (system description), #195
  (`CompartmentSupervisor`), #197 (ADR 0009 Amendment 2). 0.16: #200 (`id_effect_attest`), #201
  (placement), #202 (vault).
- nixos-dotfiles#419 (compartments epic, owner decisions 2026-11-27). Research:
  `docs/research/compartments/00-synthesis.md`, `03-prior-art-policy.md`,
  `04-build-attest-mesh.md`.
- ADR 0009 (process mesh), ADR 0013 (secure by default), ADR 0014 (one call surface), ADR 0017
  (portable by default); `THREAT-MODEL.md` B4.
- Open DICE specification
  (<https://pigweed.googlesource.com/open-dice/+/refs/heads/main/docs/specification.md>); TCG DICE
  Attestation Architecture (`DiceTcbInfo`); RFC 9334 (RATS).
