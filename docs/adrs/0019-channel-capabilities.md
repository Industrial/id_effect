# ADR 0019 — Channel capabilities

## Status

Proposed (2026-11-27). Release 0.15.0 "Compartments I: Sealed", epic
[#179](https://github.com/Industrial/id_effect/issues/179), leaf `c15-adr0019-channel-caps`
([#188](https://github.com/Industrial/id_effect/issues/188)). Contract identity and digests are
leaf `c15-contract-identity` ([#189](https://github.com/Industrial/id_effect/issues/189)), and §7
fixes their shape. The owner moves this ADR to Accepted. Until then every 0.15 leaf that cites it
cites it as Proposed.

It builds on [ADR 0018](0018-heterogeneous-mesh.md), which makes reachability a matter of
capability and supplies the attested identities that capabilities are bound to. It also builds on
[ADR 0014](0014-one-call-surface.md) (RPC as the call surface; the contract artifact, §9) and
[ADR 0011](0011-fiber-runtime-send-typed-r.md) (typed `R`, Layers). Revocation semantics are in
[ADR 0009 Amendment 2](0009-process-mesh.md#amendment-2-2026-11-27-revocation-and-certificate-rotation).

Owner decisions already taken, and not re-asked here: Cap'n Proto / OCapN semantics over
`id_effect_rpc`; no reachability by pid alone; pass capabilities, not names (the issue text of
#188); RPC is the first-party call surface (ADR 0014, L15); no live migration, and DICE identities
with no fleet CA (ADR 0018).

## Context

At v0.13.0 (`origin/main`) authority in the mesh is ambient. It comes from knowing a pid, a name or
a node name:

- **Pids.** A process envelope addresses its target by `PidWire`, and the receiver delivers to it
  whoever sent the frame (`crates/id_effect_node/src/mesh.rs:759-790`; senders at `:474-496`).
- **Names.** RPC clients find a service by its cluster-wide name, `whereis(endpoint_name(service))`
  (`crates/id_effect_rpc/src/client/mesh.rs:289`; `endpoint_name` at
  `crates/id_effect_rpc/src/transport/mesh.rs:150`; `crates/id_effect_node/src/global_registry.rs:288`).
- **Node names.** The only mesh guard admits by `name@host` string
  (`MeshPeerGuard`, `crates/id_effect_rpc/src/transport/mesh.rs:296-321`). The verified principal
  is `RpcPrincipal("mesh:<node>")` (`:31-34`, `:124`). Nothing says what the caller may do beyond
  "it is node X".
- **Untyped serving.** `MeshRpc::serve(service, group, env: DynEnv)`
  (`crates/id_effect_rpc/src/transport/mesh.rs:646`) runs every call in a `DynEnv`. That is the
  fallible escape hatch of ADR 0011, not typed `R`.
- **Contracts have no identity.** `ServiceContract` has a name and a `version` (≥ 1, served side
  by side; `crates/id_effect_rpc/src/contract.rs:198-216`) but no digest. The compatibility rules
  of ADR 0014 §9 live in the CLI (`crates/id_effect_tool/src/rpc/check.rs:455`, `compare`), so a
  builder cannot call them as a library.
- **Access is per procedure, not per holder.** `ProcedureAccess` is `Public` or `Guarded{policies}`
  (`crates/id_effect_rpc/src/contract.rs:103-112`), the same for every caller that passes the
  guard.
- **Good parts to keep:** the ingress stamps each message with the authenticated origin under a
  node-local key, so a forged origin is dropped (`transport/mesh.rs:17-28`, `:607`, `:685`). Per
  peer, there are call limits, deadlines, credits and heartbeats (`:36-54`). Every input is
  Schema-decoded. The `#[rpc::service]` macro generates a typed client over any transport
  (`<Trait>Client<T>`, `crates/id_effect_rpc_macros/src/service.rs:725`, `:823`).

On `R`: since 0.7 `R` is a typed HList checked at compile time (ADR 0011 §"Typed R"; ADR 0003
Amendment 1). It names the **kind** of authority a program needs (a service trait), not **which
instance** or **how much** (research report 03 §0). Authority captured inside a Layer disappears
from its consumers' `R`. Rust is not an object-capability language: `std::fs`, `std::net`, FFI and
`DynEnv` are ambient. `R` can propose a policy; the compartment boundary and the capabilities below
enforce it (#194 makes `R` approximately sound inside a compartment).

Prior art (report 03 §3, report 04 §4.3): Cap'n Proto RPC makes interface references capabilities
held per connection. They die with the connection, and authority is by designation, so there is
no confused deputy. OCapN adds **signed three-party handoffs** that only the intended recipient can
redeem. Fuchsia routes typed capabilities with `use`/`offer`/`expose`. seL4 Microkit describes
every channel statically. The research recommends a hybrid: **static roots, attenuated passing**.

## Decision

### 1. Three kinds of capability

| Kind | Created by | Lifetime | Carried as |
|---|---|---|---|
| **Bootstrap** | the system description (Voltaic, nixos-dotfiles#417). It is in the compartment's manifest, so it is in the measurement. | as long as the wiring; survives restarts of either side | a `(role, contract digest, rights)` entry the host manager hands to the compartment at boot (#196) |
| **Live** | the exporting node, when it returns a capability from a call or narrows one it holds | the session: it dies when the session closes (`noconnection`), on revocation (`revoked`) or on release | an entry in the session's **export table** (Cap'n Proto style), named on the wire by a session-local `CapId` |
| **Handoff** | a holder (the *gifter*) passing a live or bootstrap capability to a third node | single use, TTL-bound; once redeemed it becomes a live capability on the recipient's own session with the exporter | a signed handoff certificate (§4) |

- **A bootstrap capability names a role, not a pid or a node.** For example, "the `Files` v1
  service of role `storage`, attenuated to the `read` facet". The host manager resolves the role to
  the current instance and re-resolves it after a crash-only restart or a hand-off (ADR 0018 §9).
  This is what makes a restart transparent to holders.
- **There are no persistent bearer references.** OCapN's sturdyrefs (and Cap'n Proto level 2
  persistent capabilities) are strings that anyone holding them can redeem, which is ambient
  authority again. Persistence is the job of bootstrap capabilities in the measured wiring. After a
  restart, a holder asks again the party that gave it a live capability.

### 2. Typed capabilities

```rust
pub struct ChannelCap<S: Service, F: Facet<S> = Full> { /* session, CapId, caveats */ }
pub struct ProcessCap<M: WireMessage> { /* session, CapId, caveats */ }   // raw mailbox, process layer
pub struct SpawnCap<B: Behavior> { /* session, CapId, caveats */ }        // ADR 0018 §1
```

- `S` is the marker type that `#[rpc::service]` generates for a service trait. The capability
  checks at compile time that the contract it opens is `S`'s. A call through it is a call through
  the generated `<Trait>Client`, so the procedure set, inputs, outputs and errors are typed end to
  end.
- **Facets** narrow a capability at compile time. `#[rpc::facet(Files: read = [get, stat, list])]`
  generates `FilesRead: Facet<Files>`, and `ChannelCap<Files, FilesRead>` exposes only those
  procedures. Narrowing from `Full` to a facet is free and checked at compile time. Calling a
  procedure outside the facet does not compile. The exporter also enforces the facet at run time,
  because the holder's binary is not trusted.
- **Capabilities in contracts.** A procedure input or output may contain a capability field. The
  contract JSON records it as
  `{"x-id-effect-cap": {"service": "Files", "major": 1, "facet": "read", "digest": "sha256:…"}}`.
  The wire carries a capability descriptor (a `CapId` in the sender's export table, or a handoff
  certificate). The decoder yields a `ChannelCap` only after it has checked the descriptor against
  the contract digest. This is how dynamic leaves work: "open a file" returns a `ChannelCap<File>`
  for that one file.
- `ProcessCap<M>` keeps the process layer (mailboxes, `cast`, `call`, monitors, links) under the
  same reachability rule as RPC. RPC stays the default surface (ADR 0014). `ProcessCap` exists so
  that ADR 0009 programs keep their shape.

### 3. Narrowing (attenuation)

A capability carries **caveats**. Narrowing yields a new capability whose caveats are at least as
strict as the old one's. It never widens:

| Caveat | Meaning | Narrowing rule |
|---|---|---|
| `facet` | the procedures it may call | subset |
| `not_after` | expiry (on the exporter's clock) | earlier or equal |
| `max_uses` | calls (or messages) it may make | smaller or equal |
| `rate` | token-bucket rate and burst | lower or equal |
| `min_tier` | the lowest `TrustTier` (ADR 0018 §6) a holder may have | higher or equal |
| `holder` | the identity key it is bound to (set on handoff) | set once; never changed |
| `scope` | a typed value the contract declares per service (for example a path prefix), checked by the exporter's guard | the contract's `narrows(old, new)` predicate, generated from a `#[rpc::scope]` type that implements `Narrow` |

- The exporter enforces every caveat on every call. A caveat that fails is the typed
  `CapError::Denied { caveat }`. An expired capability is `CapError::Expired`.
- Narrowing a **live** capability is a request to the exporter (`Narrow{cap, caveats}`). The
  exporter answers with a new `CapId` in the same export table, a child of the old one in the
  **derivation tree** (§8). No cryptography is needed, because the exporter holds the state. This
  is Cap'n Proto's membrane, done by the exporter itself.
- Narrowing for a **handoff** is written into the deposit (§4) and checked against the gifter's own
  caveats.
- Caveats are a fixed set plus the typed `scope`. There is no general caveat language (macaroons,
  Biscuit Datalog): a fixed set can be checked at a known cost, and its narrowing rule can be
  proved by a property test.

### 4. Signed three-party handoff

Node A (the gifter) holds a capability `c` to an object exported by node B. A wants node C (the
recipient) to hold a narrowed `c′`, without proxying C's traffic through itself and without
trusting C's word.

```text
A ──(A↔B session)──▶ B : DepositGift { gift_id, cap: c, caveats: c′ ⊆ c, recipient: key(C),
                                       not_after, revocable_by: A? }
                     B : checks A holds c on this session, c′ narrows c, recipient tier ≥ min_tier;
                         stores the gift (bounded table), answers GiftDeposited
A ──(A↔C session)──▶ C : a message whose capability field is
                         HandoffGive { exporter: id(B), gift_id, recipient: key(C), not_after,
                                       sig_A over (domain ‖ all fields ‖ id(A)) }
C ──(C↔B session)──▶ B : WithdrawGift { give, sig_C over (domain ‖ give ‖ B's TLS exporter
                                                        secret on the C↔B session) }
                     B : verifies sig_A with A's key, which B knows from the A↔B session;
                         verifies the session peer is key(C) and sig_C binds this session;
                         gift_id unused and not expired → consume it;
                         export c′ on the C↔B session → GiftWithdrawn { cap: CapId }
```

- **Only the intended recipient can redeem it.** The give certificate names C's identity key.
  B redeems it only on a mutually authenticated session whose peer holds that key, and C's
  signature binds the redemption to that session's TLS channel. A stolen give certificate is
  useless.
- **Single use.** `gift_id` is 128-bit random. B consumes it on withdrawal and remembers consumed
  ids until `not_after`. The table is bounded (§9).
- **No proxy.** After withdrawal C talks to B directly. A sees none of that traffic, unlike
  two-party Cap'n Proto, where A would have to proxy.
- **Revocable by the gifter, if asked.** With `revocable_by: A`, B attaches c′ under c in the
  derivation tree, so revoking c (or A revoking the gift) revokes c′ too.
- **Both orders work.** C may try to withdraw before A's deposit reaches B. Then B holds the
  withdrawal for up to `gift.wait` (default 5 s) and fails it with `CapError::UnknownGift` after
  that. This follows OCapN's handoff, which tolerates the race.
- **Bootstrap capabilities pass the same way.** The gifter's authority is then its bootstrap entry,
  which B checks against the wiring.

### 5. No reachability by pid alone

- The mesh frames that carry user traffic name a `CapId` in the receiver's export table for the
  sending session: `CapCast`, `CapInfo`, `CapCall`, `CapMonitor`, `CapLink`, and the RPC frames on
  the mesh transport. The exporter maps the `CapId` to its target pid internally, and the holder
  never learns that pid.
- `ProcessEnvelope::{Cast, Info, Call, Monitor, Link}` with a bare `PidWire`
  (`crates/id_effect_node/src/envelope.rs:253`) are refused with `RemoteFault::NoCapability`.
  The exception is the same-image shim `Mesh::dangerous_ambient_pids()` (ADR 0018 §10).
- New wire messages: `Narrow`, `Release{cap, count}` (Cap'n Proto release; the export entry is
  freed when its count reaches zero), `Revoked{cap, cause}`, `DepositGift`, `GiftDeposited`,
  `WithdrawGift`, `GiftWithdrawn`. They are part of mesh protocol 4 (ADR 0018 §5).
- A `Pid` may still travel as data (for logs and correlation). Holding one grants nothing.

### 6. How capabilities map onto `R`, Layers and the RPC contract

**`R` names the shape, the wiring picks the instance, and the caveats set the amount** (report 03,
options A, B and C).

- **A program keeps its `R`.** A program needs `caps!(Files, Clock)` as before. `Files` is the
  service trait. Its business code cannot tell whether `Files` is in-process or across a
  compartment boundary.
- **The compartment root provides remote-backed Layers.** For every capability in the residual
  `R` that the manifest wires to another compartment, the root provides
  `Files::remote_layer(role)`. That layer has type
  `Layer<Files, CapError, caps!(Bootstrap)>`: it builds a `<Files>Client` over the bootstrap
  `ChannelCap<Files, F>` for `role`. Tests provide the in-process Layer instead, with the same `R`.
  This is how "testable in-process with the same `R`" holds (report 03, option C).
- **The remote error type is visible.** A remote-backed service's methods can fail with
  `CapError` (`Unavailable`, `Revoked`, `Expired`, `Denied`, `ContractMismatch`). Each
  `#[rpc::service]` trait that is meant to be remotable declares it in its error union, so callers
  handle it in `E`. This is Cap'n Proto's lesson from CORBA: location transparency must not hide
  failure.
- **Dynamic capabilities are values, not `R`.** A `ChannelCap` returned by a call is an ordinary
  value passed by designation (Hardy's confused deputy, removed by construction). It never enters
  `R` through `DynEnv`. #194 forbids `DynEnv` at compartment roots.
- **The residual `R` is the manifest's `use`; exported Layers are its `expose`.** #192 generates
  both. The system description adds instance and caveats (`offer`). The builder fails on an unwired
  `use`, an unused `offer` or a contract mismatch (#193, `rpc check` as a library, §7).
- **The serving side gets typed `R` and a capability principal.**
  - `MeshRpc::serve_layer::<S, R>(layer)` replaces `serve(.., env: DynEnv)`
    (`transport/mesh.rs:646`), which stays one minor as a deprecated shim.
  - A guard sees `CapPrincipal { holder: VerifiedPeer, cap: CapId, caveats }` in the request
    extensions, in place of `MeshPeer`.
  - `CapGuard` admits a call when the capability's facet contains the procedure and every caveat
    holds. It is the default guard for procedures served on the mesh. `#[guard(MeshPeerGuard)]` is
    deprecated (ADR 0018, M4).
- **The contract records capabilities.** `ProcedureAccess` (`contract.rs:103-112`) gains
  `Capability { facet }`, meaning that only holders of a capability whose facet includes the
  procedure may call it. `rpc check` treats removing a procedure from a published facet as
  breaking, and adding one as compatible.

### 7. Contract identity and digests (#189)

- **`ContractDigest`** is the SHA-256 of one `ServiceContract` in its canonical JSON, with the
  domain prefix `id-effect.rpc.contract-digest/1\0`. The canonical form is the existing stable
  serialisation (`contract.rs:1-7`: sorted, two-space indentation, trailing newline), with every
  `description` member removed. Documentation edits therefore do not change a measurement, and
  every schema change does.
- **`ContractId = (service name, major)`**, where major is today's `ServiceContract.version`. It
  says what is substitutable. The digest pins the exact contract.
- **`revision: u32`** is a new member of `ServiceContract`, 0 by default. `rpc check` requires a
  higher revision whenever the digest changes under the same major and the change is compatible.
  Under the same major, a change that is not compatible stays a failure, as in ADR 0014 §9.
- **Per-channel negotiation** (ADR 0018 §5). Each side states `{name, major, revision, digest}`.
  The channel opens when the pair is in the measured wiring and the peer's certificate lists its
  digest.
- **`rpc check` as a library.** `id_effect_rpc::contract::check(previous, current) -> CheckReport`
  moves from `id_effect_tool::rpc::check::compare` (`crates/id_effect_tool/src/rpc/check.rs:455`),
  and the CLI calls the library. The system builder (nixos-dotfiles#417) calls it to verify that
  the channels in a compartment manifest are a subset of the checked contracts.
- **Capability types are part of the digest.** A capability field's schema node names its service,
  major, facet and digest, so changing the contract of a capability a procedure returns changes the
  digest of the contract that returns it.

### 8. Revocation

- **Every capability has a revoker.** The exporter holds it, and so does the gifter if it deposited
  with `revocable_by`. `CapRevoker::revoke()` revokes the capability and **its whole derivation
  subtree**: every narrowing and every handoff made from it.
- **Revocation by identity.** When a holder's certificate expires unrenewed, or its root or
  measurement is removed (ADR 0018 §3), the exporter revokes every capability held by that
  identity.
- The exporter is the enforcer, so revocation takes effect there at once: calls after it are
  refused. Holders learn of it from `Revoked{cap, cause}`, or at their next call. In-flight calls
  fail with `CapError::Revoked`. Monitors and links set up through the capability fire with reason
  `revoked`, exactly once. Pending reliable sends fail with `DeliveryFailure::Revoked`. The tables
  are in ADR 0009 Amendment 2.
- A revoked capability stays revoked. Supervisors must not restart a child on the same revoked
  capability. A restart asks for a new grant.

### 9. Bounds (ADR 0013 R4)

All bounds are finite by default and configurable:

| Bound | Default |
|---|---|
| export table entries per session | 4,096 |
| derivation depth (narrowings of narrowings) | 16 |
| live gifts held per exporter | 1,024 |
| consumed `gift_id`s remembered (until `not_after`) | 65,536 |
| gift TTL | 5 minutes |
| `gift.wait` for an early withdrawal | 5 s |
| capability descriptors per message | 64 |

Over a bound is `CapError::ResourceExhausted`, counted, and the session stays up. Each new decoder
(capability descriptor, handoff certificate, the new frames) gets a fuzz target.

### 10. What this ADR does not do

- **Promise pipelining** (Cap'n Proto level 1 "call on a promised result"). Deferred: it saves
  round trips, but makes cancellation and the deadline model of ADR 0014 §2 more complex. Open
  question 1.
- **Capability equality** (Cap'n Proto level 4 "join"). A non-goal.
- **Information-flow labels** on capabilities. A later ADR (report 03 §5).
- **Wire compatibility with Cap'n Proto or OCapN.** The semantics follow them. The wire is
  id_effect's postcard RPC frame (ADR 0014 §10).

## Threat model

The actors are ADR 0018's: **A1** a compromised leaf, **A2** an on-path attacker, **A3** a
compromised host, **A4** an operator mistake.

| # | STRIDE | Threat | Mitigation | Enforcement |
|---|---|---|---|---|
| C1 | E | **Confused deputy**: A1 tricks a service into using the service's own authority on a resource A1 names. | Designation: a service acts on capabilities the caller passes, not on names. Contracts carry capability fields (§2). | C-E1, C-E2 |
| C2 | E | A1 guesses or replays a `CapId`. | `CapId`s are per session and meaningless on any other session. The exporter resolves them only in the sending session's table. | C-E3 |
| C3 | E | A1 widens a capability. | Narrowing is computed and enforced by the exporter. Facets are checked at compile time and again at run time. The narrowing rules are property-tested. | C-E4, C-E5 |
| C4 | S | A1 redeems a handoff meant for someone else, or redeems one twice. | The give certificate names the recipient's key. Redemption is bound to the recipient's TLS session. `gift_id`s are single use. | C-E6, C-E7 |
| C5 | S | A1 forges a gift from a node that never gave one. | B verifies the gifter's signature against the gifter's identity from B's own session with it, and checks that the gifter holds the capability. | C-E8 |
| C6 | E | A1 hands a capability to a lower-tier or unwired identity. | `min_tier` is checked at deposit, at withdrawal and at every call. A bootstrap capability's wiring limits who may hold it. | C-E9 |
| C7 | E | A1 keeps using authority after revocation. | The exporter enforces revocation at once, including the derivation subtree. | C-E10 |
| C8 | D | A1 exhausts export tables, gift tables or derivation depth. | The bounds of §9. | C-E11 |
| C9 | T | Hostile capability descriptors or certificates. | Schema decode, fuzz targets. | C-E12 |
| C10 | E | A1 and C collude to pass authority on. | Accepted. Colluders can always proxy, and no capability system prevents that. Delegation is still bounded by `min_tier` and the wiring, and revocation still reaches the subtree. | — (accepted) |
| C11 | E | A3 (the exporter's own host) ignores caveats. | Accepted: the exporter is in its host's TCB. Callers treat outputs as untrusted (Schema). | — (accepted, ADR 0018 T10) |

## Enforcement points

These follow the ADR 0013 style: each rule holds while its enforcement point is green. Test ids
start with the ADR 0013 rule they enforce.

| # | Rule | ADR 0013 | Enforcement point (mechanical) | Leaf |
|---|---|---|---|---|
| C-E1 | A remote service is reachable only through a capability. A name lookup grants nothing. | R1 | `r1_rpc_mesh_call_without_capability_refused`, `r1_whereis_from_leaf_returns_no_capability` | #188 |
| C-E2 | A procedure guarded by `Capability{facet}` refuses holders of other facets. | R1 | `r1_capability_procedure_outside_facet_refused` | #188 |
| C-E3 | A `CapId` from another session is refused. | R1 | `r1_cap_id_replayed_on_other_session_refused` | #188 |
| C-E4 | Calling outside a facet does not compile. | R1 | compile-fail `r1_call_outside_facet` (`tests/security_compile_fail`) | #188 |
| C-E5 | Narrowing never widens. | R1 | property test `narrowing_is_monotone` (every caveat, every `Narrow` scope); `r1_narrow_request_widening_refused` | #188 |
| C-E6 | A handoff redeemed by another identity is refused. | R8 | `r8_handoff_withdrawn_by_wrong_key_refused`, `r8_handoff_withdraw_signature_bound_to_session` | #188 |
| C-E7 | A handoff is single use and expires. | R1 | `r1_handoff_redeemed_twice_refused`, `r1_handoff_after_not_after_refused` | #188 |
| C-E8 | A gift whose gifter signature is wrong, or whose gifter lacks the capability, is refused. | R8 | `r8_forged_handoff_give_refused`, `r1_deposit_of_unheld_capability_refused` | #188 |
| C-E9 | `min_tier` is enforced at deposit, withdrawal and call. | R1 | `r1_handoff_to_lower_tier_refused`, `r1_call_below_min_tier_refused` | #188 |
| C-E10 | Revocation cuts the derivation subtree at once. | R1 | `r1_revoke_cuts_narrowed_and_handed_off_caps`, `r1_call_after_revoke_refused` (TestExecutor, in-memory transport) | #188, #197 |
| C-E11 | Capability state is bounded. | R4 | `r4_export_table_bound`, `r4_derivation_depth_bound`, `r4_gift_table_bound` | #188 |
| C-E12 | New decoders are fuzzed. | R4 | fuzz targets `cap_descriptor`, `handoff_certificate` in ClusterFuzzLite, no open crash | #188 |
| C-E13 | Contract digests are stable over descriptions and change with every schema change. | — | `contract_digest_ignores_descriptions`, `contract_digest_changes_on_schema_change`, golden `rpc.contract.json` digests | #189 |
| C-E14 | `rpc check` is a library and enforces `revision`. | — | `check_requires_revision_bump_on_compatible_change`; the CLI's existing `check` tests pass through the library | #189 |
| C-E15 | Ambient pids are a recorded opt-out that the prod posture refuses. | R10 | `dangerous_ambient_pids_is_recorded`, `posture_prod_refuses_ambient_pids`; `scripts/check-dangerous.sh` | #188 |

## Migration from ADR 0009 and ADR 0014

| From (0.14) | To (0.15) | Shim (one minor, L9) |
|---|---|---|
| `Mesh::remote_cast(&Pid, ..)`, `remote_info`, `remote_call`, `remote_monitor` | `ProcessCap<M>::cast`, `call` and `monitor` | `Mesh::dangerous_ambient_pids()` for same-image meshes, recorded |
| `whereis(endpoint_name(S))` in `client::mesh` | a bootstrap `ChannelCap<S>` from the manifest, or one received in a call | name lookup works only among host managers, and under the shim |
| `MeshRpc::serve(service, group, DynEnv)` | `MeshRpc::serve_layer::<S, R>(layer)` | `serve` `#[deprecated]` |
| `#[guard(MeshPeerGuard::nodes([..]))]` | `#[capability(facet = ..)]` with `CapGuard` | `MeshPeerGuard` `#[deprecated]` |
| `ServiceContract { name, version }` | `+ revision`, digest computed | `revision` defaults to 0; old contract files still parse |
| `id-effect rpc check` (CLI only) | `id_effect_rpc::contract::check` (library); the CLI calls it | — |

`docs/MIGRATION-0.15.md` gets one entry per row. The Relay and Ledger reference apps and the
`cluster` template move in 0.15, and the Warden exit KPI
([#198](https://github.com/Industrial/id_effect/issues/198)) runs every agent compartment on
bootstrap capabilities only.

## Consequences

Positive:

- Authority is explicit, typed and countable. A compartment's reach is its bootstrap table plus
  what it was handed, and each piece can be narrowed and revoked.
- The confused deputy is removed by construction for services that take capability arguments.
- Program code keeps its `R`. The compartment boundary is a Layer swap, and in-process tests stay
  valid.
- Contract digests make "the peer runs the contract it says" checkable from the certificate, and
  give the system builder something exact to verify.

Negative:

- Every mesh user has to migrate send-by-pid, name lookup and node-name guards. The shims cost one
  minor.
- The exporter keeps per-session state for exports, derivation trees and gifts. It is bounded, but
  it is not free.
- Without promise pipelining, a chain of calls that each return a capability costs a round trip per
  link.
- Services meant to be remotable must put `CapError` in their error unions, which adds a variant to
  their callers' `E`.

## Alternatives considered

- **Static wiring only (Microkit / Ghaf style).** Rejected: it cannot pass one file or one session
  to a peer, and the crash-only hand-off (ADR 0018 §9) needs a one-off capability.
- **Capabilities as bearer tokens (macaroons, Biscuit, sturdyrefs).** Rejected as the mechanism:
  anyone holding the token is authorised, so a leak is a grant. Caveats in the macaroon style are
  kept, but they are bound to a holder key and enforced by the exporter.
- **Two-party Cap'n Proto only (A proxies for C).** Rejected: A would see and could tamper with all
  of C's traffic, and would become a bottleneck and a deputy.
- **Adopt `capnp-rpc` and the Cap'n Proto IDL.** Rejected: it would add a second IDL beside the
  RPC contract (ADR 0014 rejected gRPC for the same reason), and the Rust implementation has no
  three-party handoff. The semantics are adopted; the wire stays id_effect's.
- **Name-based policy (Qubes qrexec style).** Rejected: report 03 §3.2 lesson 3. Names need a
  global policy and invite the confused deputy.
- **Put instance and amount in `R` (indexed capability types, report 03 option B′).** Not the
  default: it pushes topology into program code. It stays possible for programs that want it,
  because a facet is a type.

## Open questions (defaults taken)

1. **Promise pipelining in 0.15?** Default: no. Revisit when a measured chain of calls needs it.
2. **Caveat set.** Default: the fixed set of §3, plus one typed `scope` per service. A general
   caveat language is not offered.
3. **Default bounds** (§9). Default: as written. They are per session, and they are configurable.
4. **Does the digest cover descriptions?** Default: no, so that documentation edits do not change
   measurements. The alternative pins docs too, at the cost of rebuilding images for typo fixes.
5. **`revision` in the contract file.** Default: a new `revision` member that `rpc check`
   enforces. The alternative, deriving it from history, needs the previous contract at build time.
6. **`ProcessCap` for raw mailboxes.** Default: yes, so that ADR 0009 programs keep their shape. The
   alternative is RPC only on the mesh.
7. **`gift.wait` for early withdrawals.** Default: 5 s.

## References

- Epic #179. Leaves: #188 (this ADR and its implementation), #189 (contract identity), #192
  (capability manifest), #193 (system description), #194 (ambient lints), #196 (`compartment-init`),
  #197 (ADR 0009 Amendment 2), #198 (exit KPI).
- [ADR 0018](0018-heterogeneous-mesh.md), [ADR 0014](0014-one-call-surface.md) §9–§10 and §14,
  [ADR 0011](0011-fiber-runtime-send-typed-r.md), ADR 0003 Amendment 1, ADR 0013.
- nixos-dotfiles research: `docs/research/compartments/03-prior-art-policy.md` (§0, §3, §4 options
  A–G), `04-build-attest-mesh.md` (§4.3, §4.4).
- Cap'n Proto RPC protocol (<https://capnproto.org/rpc.html>); OCapN and CapTP
  (<https://github.com/ocapn/ocapn>); Hardy, "The Confused Deputy"
  (<http://cap-lore.com/CapTheory/ConfusedDeputy.html>); Miller, "Robust Composition"
  (<http://www.erights.org/talks/thesis/>).
