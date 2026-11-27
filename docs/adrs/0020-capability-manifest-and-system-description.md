# ADR 0020 — Capability manifests from `R`, and a checked flat system description

## Status

**Proposed** (2026-11-27). Release 0.15.0 "Compartments I: Sealed" (epic #179), leaves
`c15-cap-manifest` (#192) and `c15-system-description` (#193). The owner moves it to Accepted.

Scope: what a compiled id_effect program exports about its authority, how a topology routes that
authority, the flat description that results, and the small checker that gates it. These are out
of scope and covered elsewhere:

- the mesh without the same-binary assumption: ADR 0018 (#187);
- channel capabilities, attenuation, revocation and three-party handoff: ADR 0019 (#188);
- contract digests and per-channel version negotiation: #189;
- lints against ambient std authority, and generated seccomp/Landlock: #194;
- run-time quota donation and unforgeable session labels: #203 (0.16.0);
- information-flow analysis over the whole Layer graph: #204 (0.16.0).

The number 0020 follows 0018 and 0019, which are being written in parallel. `release/0.14.0`
uses 0016, and 0017 is "Portable by default".

## Context

The compartments programme (nixos-dotfiles #419; synthesis in nixos-dotfiles
`docs/research/compartments/00-synthesis.md`) runs every program in its own hardware-isolated
compartment. A program reaches only the typed channels it has been granted, and policy is
**derived from types**. The owner decisions of 2026-11-27 that this ADR builds on:

- one compartment per program;
- foreign software gets hand-written capability types, so its policy is still derived from types
  (nixos-dotfiles #426);
- the trusted computing base (TCB) should be as small as possible. Start on KVM with
  cloud-hypervisor and target seL4 Microkit, with **one system description and both backends from
  day one** (report 01 §5);
- topology and images are Voltaic build outputs (nixos-dotfiles #417, #422).

What id_effect already has that this ADR uses:

- **`R` is a typed HList** (ADR 0003 Amendment 1, ADR 0011). Membership is checked at compile time,
  and only `Effect<_, _, HNil>` runs. So the `R` left on a program's entry effect, before the edge
  provides it, is a compile-time fact: it is exactly the authority the program needs from outside
  its own Layer graph.
- **A capability is identified by its Rust type.** Two instances of one service need two newtypes.
  `TypeId` is not stable across builds, so a type alone cannot name a capability between
  binaries.
- **Layers are values**, `Layer<Out, E, In>`. A Layer composed with `provide_to`/`merge` carries in
  `In` the external capabilities it still needs.
- **`id_effect_rpc` has canonical contracts** (`id-effect.rpc.contract/1`: sorted JSON, with golden
  snapshots checked by `assert_snapshot`). #189 adds a SHA-256 digest over that canonical form.
- **ADR 0013** gives the posture registry (`record_dangerous`) and the format "one rule, one
  mechanical enforcement point" used below.

Report 03 (§0, §4) says what `R` does and does not express:

1. `R` names the **kind** of authority (`HttpClient`). It does not say **which instance** provides
   it (the egress proxy for `api.example.com`) or **how much** is allowed (GET only, 8 requests in
   flight).
2. `R` hides authority **captured inside a Layer**. A `Signer` built from `caps!(Keystore, Http)`
   removes both from the `R` of a program that uses `Signer`.
3. Rust has **ambient authority**. `std::fs`, `std::net`, `std::process`, `std::env`, `unsafe`, FFI
   and `DynEnv` reach the world without appearing in `R`. Types propose; the boundary disposes.

The systems in report 03 that got least privilege right share three traits. They keep kind,
instance and restriction apart (Fuchsia). They check routes at build time (Fuchsia `scrutiny`).
And they compile policy to a flat, checkable artefact that a small trusted step realises
(CAmkES → capDL → initialiser). The anti-patterns are hand-written manifests that drift (Android,
Fuchsia CML beside the code) and policy fetched at run time (Ghaf).

## Decision

The pipeline:

```
 id_effect binary              Voltaic topology (#417)
 entry Effect<_, _, R>         compartments, offers,
 .note.id_effect.manifest ──►  restrictions, labels,  ──lower──►  system.json  ──check──►  realiser
       (extracted)             policy allowlists      (generator,  (flat,        (small,     ├─ KVM (nd#423)
 hand-written foreign ─────►                          untrusted)   canonical)    independent) └─ seL4 Microkit .system
 manifest (nd#426)
```

The generator is not trusted. The checker is, and each realiser runs it itself before acting.

### 1. The manifest from `R`

#### 1.1 Capability descriptors

A capability that may cross a compartment boundary, or that the compartment runtime grants, has a
stable descriptor:

```rust
pub struct Descriptor {
  pub package: &'static str,       // "id_effect_auth": the owning crate
  pub name: &'static str,          // "Keystore"
  pub major: u32,                  // substitutability, by the rpc `check` rules (#189)
  pub minor: u32,                  // additive features
  pub contract: Option<[u8; 32]>,  // SHA-256 of the canonical contract JSON; None for runtime grants
  pub grant: Grant,                // Remote (a channel) | Runtime(Primitive): clock, entropy, log sink
}

pub trait CapabilityDescriptor { const DESCRIPTOR: Descriptor; }
```

The attribute
`#[id_effect::capability(package = …, name = …, major = …, contract = "contracts/keystore.v1.json")]`
implements the trait. A `const fn` SHA-256 computes the contract digest at compile time, over
`include_bytes!` of the **committed golden snapshot**. The existing `assert_snapshot` test already
pins that snapshot to the trait's real contract. Two mechanical checks therefore tie the digest in
the binary to the code's contract.

The canonical text form is `package/name@major.minor`, for example `id_effect_auth/Keystore@1.0`.
In a compartment entry, a type in `R` without a descriptor is a compile error. Its
`on_unimplemented` message reads: "`T` is in the compartment's `R` but has no capability
descriptor; add `#[id_effect::capability]` or provide it locally".

#### 1.2 Extraction mechanism: const encoding into a linker section

Three mechanisms were weighed:

| Mechanism | Sees the resolved `R`? | Describes the artefact that ships? | Verdict |
|---|---|---|---|
| Proc-macro alone | No. A macro sees tokens, not resolved types, and not the `R` left after `provide` | Only indirectly | Rejected as the extractor. Kept as the thin front that emits the static |
| `build.rs`, or a test that prints the manifest | No. `build.rs` runs before the crate compiles. A test sees a different artefact (test cfg, other features) | No | Rejected. Kept for development as `Manifest::to_json()` at run time |
| **`const` encoding into a linker section** | **Yes.** The compiler evaluates trait consts over the concrete `R` | **Yes.** It is read from the binary that runs | **Chosen** |

How it works:

- `trait Manifest` is implemented for `HNil` and for `HCons<H: CapabilityDescriptor, T: Manifest>`.
  It provides `const ENCODED_LEN: usize` and a `const fn encode() -> [u8; ENCODED_LEN]`, which walks
  a `&'static` const linked list of descriptors. All of this is stable Rust: associated consts, and
  an array length taken from a concrete type's associated const in a non-generic static. A
  duplicate descriptor is a `panic!` during const evaluation, which is a compile error.
- The attribute `#[id_effect::compartment]` on the entry function names the return type. It can
  therefore project `R` without resolving anything itself:

  ```rust
  #[id_effect::compartment(expose = [signer_layer])]
  fn program() -> Effect<Never, AppError, caps!(Keystore, HttpClient, Logger)> { … }

  // generated (sketch):
  #[used]
  #[link_section = ".note.id_effect.manifest"]
  static MANIFEST: [u8; manifest_len::<ProgramR, ProgramExposes>()] =
    encode::<ProgramR, ProgramExposes>();
  fn main() { compartment_init::run(&MANIFEST, program, bootstrap_layers::<ProgramR>()) }
  ```

- The generated `main` passes `&MANIFEST` to `compartment-init` (#196). The section is therefore
  referenced, and survives `--gc-sections` and LTO. At boot, `compartment-init` hashes the section
  and refuses to start if the digest differs from the manifest digest on the measured kernel
  command line (report 04 §0). A binary cannot run under any manifest but its own.
- `id-effect manifest extract <binary>` (in `id_effect_cli`) reads the section with the `object`
  crate and prints the canonical JSON. Voltaic calls the same code as a library.
- On ELF the section is `.note.id_effect.manifest`. On wasm32 it is a custom section with the same
  name. Compartment guests and Microkit protection domains are ELF. Other object formats get only
  the run-time `to_json()` (Q3).

#### 1.3 Manifest content

`use` is the entry's remaining `R`. `expose` lists the Layers named in `expose = [...]`. Each
exposed entry lists the imports it **captures**: the descriptors in the `In` of the exposed Layer
as composed. A composed Layer's `In` is what it still needs from outside, so in 0.15.0 this is the
whole answer at compartment level. #204 refines it with a full Layer-graph analysis for
information flow.

```json
{
  "format": "id-effect.manifest/1",
  "program": { "name": "signer", "origin": "derived", "build": "sha256:…" },
  "use": [
    { "capability": "id_effect_auth/Keystore@1.0", "contract": "sha256:9f2c…", "grant": "remote" },
    { "capability": "id_effect_platform/HttpClient@2.1", "contract": "sha256:41ab…", "grant": "remote" },
    { "capability": "id_effect_logger/Logger@1.0", "grant": "runtime:log-sink" }
  ],
  "expose": [
    { "capability": "app_sign/Signer@1.0", "contract": "sha256:77d0…",
      "captures": ["id_effect_auth/Keystore@1.0", "id_effect_platform/HttpClient@2.1"] }
  ],
  "dyn_env_root": false
}
```

#### 1.4 Kind, instance and restriction stay separate

| Question | Source | Where it lives |
|---|---|---|
| **Kind:** which protocol | the type in `R` | manifest `use`/`expose` (descriptor and contract digest) |
| **Instance:** who provides it | the Voltaic topology | the route's `from`: a compartment's `expose`, or a runtime primitive |
| **Restriction:** how much | the Voltaic topology and policy | the route's `restrict` (quota, procedure allowlist, host allowlist, rate), compartment labels, and `policy.restricted` |

A manifest never names an instance or a restriction, and a route never changes a kind. So the same
binary can be deployed with different authority. Indexed types (`Keystore<Deploy>`, report 03
option B′) are allowed where the scope is part of the program's meaning. They are then simply a
different kind.

#### 1.5 What `R` cannot see

- **Authority captured inside Layers.** At the boundary this is handled by construction. Any
  external capability that a local Layer needs stays in the entry's `R`, so the compartment's
  `use` list is complete. What `R` alone cannot say is *which export* carries *which import*. The
  `captures` field (§1.3) records that, for #204.
  - `Layer::from_effect` reaching into a `DynEnv` breaks the type-level account. If the root holds
    a `DynEnv`, the manifest says `"dyn_env_root": true` and every export captures every import.
    The compartment attribute also records `dangerous_dyn_env_root` in the posture registry
    (ADR 0013 R10).
  - #194 forbids `DynEnv` in compartment roots by lint.
- **Ambient std authority.** `std::fs`, `std::net`, `std::process`, `std::env`, `unsafe` and FFI do
  not appear in `R`. There are two answers, and neither trusts `R`:
  1. **The boundary.** A compartment gets no NIC, no disk and no device unless the description
     grants a channel or a region. Ambient calls inside the guest reach only what the realiser
     gave it.
  2. **Defence in depth, leaf #194:**
     - `disallowed_methods`/`disallowed_types` lints;
     - handles in the style of cap-std;
     - no `DynEnv` at roots;
     - seccomp and Landlock profiles generated from this manifest.

### 2. Topology: use/offer/expose, declared in Voltaic

The routing algebra is Fuchsia's (report 03 §1.1, §6.1):

- a compartment **uses** what its manifest says;
- it **exposes** what its manifest says;
- the topology **offers** an exposed capability, or a runtime primitive, to a user.

The topology is declared in Voltaic (nixos-dotfiles #417). id_effect supplies the types and a
library in the crate **`id_effect_topology`**.

- **Types:**
  - `Topology`, `Realm`, `Compartment` and `Host` (memory, cores, reserved budget);
  - `Image` (derivation, measurement, manifest digest);
  - `Offer` and `Restriction` (`Quota`, `ProcedureAllowlist`, `HostAllowlist`, `Rate`);
  - `Labels` (secrecy and integrity tag sets) and `Declassifier`;
  - `Policy` (restricted capabilities and their allowed consumers);
  - `Region`, for explicit shared-memory regions.
- **Library functions:**
  - `load_manifest(bytes)` parses and validates `id-effect.manifest/1`;
  - `Topology::lower(&self) -> SystemDescription` flattens realms, assigns channel ids and emits
    canonical JSON;
  - `explain(err)` maps a checker error back to the Voltaic declaration that caused it.
- **Realms** may nest, as in Genode: a compartment manager can host sub-compartments under a policy
  it cannot exceed. Lowering flattens realms, and names become path-qualified (`agents/a3`).
  An offer may go only to a child of the realm that declares it, or up to the realm's parent
  through `expose`, so wiring stays local and reviewable.

A sketch of the API that Voltaic calls (the Voltaic surface syntax is #417's to decide):

```rust
let mut t = Topology::new("drakkar", Host::memory_mib(61_000).cores(32).reserve_mib(4_096));
let vault = t.compartment("vault", img_vault).secrecy(["deploy-keys"]).integrity(["fleet"]);
let agent = t.compartment("agents/a3", img_agent);          // internet-facing: no tags
let gate  = t.compartment("sign-gateway", img_gate)
  .secrecy(["deploy-keys"]).integrity(["fleet"])
  .declassifier(Declassifier::drops_secrecy(["deploy-keys"]).adds_integrity(["fleet"]));
t.offer(id("id_effect_auth/Keystore@1")).from(vault.expose()).to(&gate)
  .restrict(Quota { max_inflight: 4, max_message_bytes: 16 << 10, queue_depth: 8, donate_kib: 64 });
t.offer(id("app_sign/Signer@1")).from(gate.expose()).to(&agent)
  .restrict(ProcedureAllowlist::only(["sign_commit"]));
t.policy().restrict(id("id_effect_auth/Keystore@1"), ["sign-gateway"]);
```

The library is the **generator**, and it may be wrong. Nothing downstream trusts it; that is the
point of §3 and §4.

### 3. The flat system description: `id-effect.system/1`

The description follows the spirit of capDL and Microkit's `.system`. No abstraction is left in
it: every compartment, region, channel, quota and label is listed. It is the **only** input either
realiser accepts.

#### 3.1 Encoding and versioning

- **Canonical JSON**, by the same rules as `id-effect.rpc.contract/1`:
  - object keys sorted, no duplicate keys, UTF-8;
  - no floats: every number is a `u64`;
  - names match `[a-z0-9][a-z0-9-]*`, with `/` as the realm separator;
  - arrays that are sets are sorted by name.

  The **description digest** is SHA-256 over the canonical bytes. It is a measurement input.
- **Versioning.** `"format": "id-effect.system/<major>"`, with no minor versions:
  - **unknown fields are rejected** (`deny_unknown_fields` everywhere), so an older checker can
    never silently ignore a field that adds authority;
  - a new field means a new major, and a checker for `/2` keeps its `/1` parser;
  - a realiser refuses any major its linked checker does not know.
- **Generator stamp.** `"generator": { "name", "version", "topology_digest" }` is informational, for
  `explain` and for audits. The checker only checks that it is well formed.

#### 3.2 Schema

```json
{
  "format": "id-effect.system/1",
  "system": "drakkar",
  "generator": { "name": "id_effect_topology", "version": "0.15.0", "topology_digest": "sha256:…" },
  "host": { "memory_mib": 61000, "cores": 32, "reserved_mib": 4096, "smt_exclusive": true },

  "labels": {
    "secrecy_tags":   ["deploy-keys", "personal", "trading"],
    "integrity_tags": ["fleet", "user"]
  },

  "compartments": [
    {
      "name": "sign-gateway",
      "origin": "derived",
      "reviewed_by": [],
      "image": { "measurement": "sha384:…", "manifest_digest": "sha256:…" },
      "manifest": { "format": "id-effect.manifest/1", "…": "embedded verbatim" },
      "resources": { "vcpus": 1, "memory_mib": 64, "cores": [4], "donatable_kib": 1024,
                     "priority": 100, "budget_us": 1000, "period_us": 1000 },
      "secrecy":   ["deploy-keys"],
      "integrity": ["fleet"],
      "declassifier": { "drops_secrecy": ["deploy-keys"], "adds_integrity": ["fleet"] },
      "devices": []
    }
  ],

  "memory_regions": [
    { "name": "blob-ring", "size": 65536, "page_size": 4096,
      "maps": [ { "compartment": "builder-1", "perms": "rw" },
                { "compartment": "builder-2", "perms": "r" } ] }
  ],

  "channels": [
    {
      "id": 7,
      "kind": "rpc",
      "capability": "app_sign/Signer@1.0",
      "contract": "sha256:77d0…",
      "procedures": ["sign_commit", "sign_tag"],
      "client": { "compartment": "agents/a3", "use": "app_sign/Signer@1.0" },
      "server": { "compartment": "sign-gateway", "expose": "app_sign/Signer@1.0" },
      "regions": [],
      "quota": { "max_inflight": 4, "max_message_bytes": 16384, "queue_depth": 8, "donate_kib": 64 },
      "restrict": { "procedures": ["sign_commit"], "hosts": [], "rate_per_s": 10 }
    }
  ],

  "runtime_grants": [
    { "compartment": "sign-gateway", "use": "id_effect_logger/Logger@1.0", "primitive": "log-sink",
      "quota": { "max_message_bytes": 4096, "rate_per_s": 100 } }
  ],

  "policy": {
    "restricted": [ { "capability": "id_effect_auth/Keystore@1", "consumers": ["sign-gateway"] } ],
    "declassifiers": ["sign-gateway"],
    "devices": []
  }
}
```

Field notes:

- `origin` is `derived` (the manifest came from a binary) or `foreign` (it was written by hand,
  §6). `reviewed_by` must be non-empty for a foreign compartment.
- `declassifier` is `null` for an ordinary compartment.
- `devices` names VFIO devices on KVM, or device regions and IRQs on Microkit.
- `kind` is `rpc` (a copying channel), `ring` (shared memory) or `notify` (a signal).
- `procedures` is the procedure-name list of the contract digest. It is carried so the checker can
  check `restrict.procedures` without parsing contracts.
- `regions` lists the regions of a `ring` channel.

**Labels** follow Flume/HiStar at compartment granularity (report 03 §2, §5.2).

- Secrecy is a set of tags. The empty set is public, and more tags means more secret.
- Integrity is a set of tags. The empty set is "internet", and more tags means more trusted.
- Data may flow from A to B when `S(A) ⊆ S(B)` and `I(B) ⊆ I(A)`.
- A **declassifier** D is a named compartment: a MILS guard, or Jif's `declassify` with authority.
  It is listed in `policy.declassifiers`, and it is the only place a label goes down. D validates
  what comes in and filters what goes out:
  - flows *out of* D are checked with `S(D) \ drops_secrecy`;
  - flows *into* D are checked with the source's integrity raised to `I(A) ∪ adds_integrity`.

  In the example, the internet-facing agent may call `sign-gateway`, and the gateway may call the
  vault, but the agent can never reach the vault, or anything else holding `deploy-keys`, any other
  way.

The direction of flow depends on the channel kind:

- `rpc` carries data **both ways** (request and response), so both directions must satisfy the
  rule (Q5).
- `ring` carries data in the direction the regions' map permissions allow: from the `rw` writer to
  the `r` reader.
- `notify` is a signal without payload. It is accepted as a one-bit-per-event channel and listed in
  the threat model (Q10).

#### 3.3 Quotas

Every channel and every runtime grant carries finite bounds: ADR 0013 R4 applies across
compartments too. `donate_kib` is Genode-style quota donation, where the client pays for the
session state it causes in the server. The checker checks the static sums, and #203 enforces
donation and session labels at run time.

### 4. The independent checker: `id_effect_syscheck`

#### 4.1 Size and independence

- **Budget:** at most **1,500 lines** of non-test Rust in total, and at most **800 lines** of check
  logic, not counting schema structs and error types. CI measures this with
  `scripts/check-syscheck-budget.sh` (tokei, tests excluded) and fails over budget. Raising the
  budget needs an amendment to this ADR.
- **No dependency on the rest of id_effect.** The checker has its own schema types and does not
  share them with `id_effect_topology`. The generator and the checker meet only on the JSON, so a
  bug in shared code cannot hide in both. This is translation validation, as in Cogent and capDL.
  CI asserts that `cargo tree -p id_effect_syscheck -e normal` contains no `id_effect*` crate.
- **Dependencies:** `serde` (derive), `serde_json` and `sha2`, nothing else (Q2). The crate is
  `no_std + alloc`, so it can run in the KVM realiser, in Voltaic, in `compartment-init`, and later
  in a seL4 protection domain.
- **Pure:** `fn check(bytes: &[u8]) -> Result<Checked, Vec<Violation>>`, with no I/O, no clock and
  no randomness. `Checked` is a newtype that only `check` can construct, holding the parsed
  description and its digest. Both backends take `Checked`, never raw bytes.
- **Errors** have stable codes (`SC001`, …) and the path of the offending element. The codes are
  part of the golden corpus and are never renumbered.

#### 4.2 Checks

Structural checks run first:

- canonical form, and unique names;
- every reference resolves;
- each `manifest_digest` equals the SHA-256 of the embedded manifest;
- a channel's `contract` equals both endpoints' contract digests;
- every `captures` entry is one of the compartment's uses.

Then the policy checks:

| Code | Check | Rule |
|---|---|---|
| SC010 | **Every `use` is satisfied** | Each `use` in each embedded manifest is matched by exactly one channel (`grant: remote`) or one runtime grant (`grant: runtime:*`). The match must have the same `package/name@major`, a minor ≥ the use's, and the same contract digest. |
| SC011 | **No unused route** | Every channel and runtime grant ends at a `use` that exists in the client's manifest. Every channel starts at an `expose` that exists in the server's manifest. A route nobody uses is rejected, and so is a route from an export the server does not have. |
| SC012 | **No route that is not allowed** | A capability in `policy.restricted` reaches only its listed consumers. `restrict.procedures` ⊆ the channel's `procedures`. A foreign compartment has a non-empty `reviewed_by`. A compartment holds `devices` only if `policy.devices` allows it. |
| SC020 | **Label flow** | Every flow (both directions for `rpc`, the map direction for `ring`) satisfies `S(A) ⊆ S(B)` and `I(B) ⊆ I(A)`, adjusted only at declared declassifiers (§3.2). Because `⊆` is transitive, the edge rule also rules out every multi-hop path: transitive exfiltration (secrets → backup → object store → internet) and integrity inversion (internet → reader → build → deploy). A path can go down only through a declassifier. |
| SC021 | Shared memory within one label | A `ring` channel, or any region mapped into two compartments, needs equal secrecy and equal integrity on both sides. Cross-label traffic uses copying `rpc` (report 03 §3.2). |
| SC030 | **Quota sanity** | Every bound is finite and non-zero, and region sizes are multiples of `page_size`. Σ compartment `memory_mib` + Σ region sizes ≤ `host.memory_mib − reserved_mib`. For each client, Σ `donate_kib` ≤ its `donatable_kib`. With `smt_exclusive`, no core is listed by two compartments. For ring channels, `max_message_bytes × queue_depth` fits the ring. `budget_us ≤ period_us`. |

#### 4.3 Who runs it

- **The Voltaic build.** The build fails on any violation, and `explain` maps each code back to its
  declaration.
- **Each realiser.** It links the checker and runs it again before it acts. It takes `Checked`, not
  a stamp, so a description that skipped the build check still cannot be realised. This is what "the
  host realiser accepts only checked descriptions" means.

### 5. Two backends from day one

Each backend is a function `Checked → backend config`. Both are small and make no policy
decisions: everything they need is in the description.

| Description element | KVM realiser (nixos-dotfiles #423, cloud-hypervisor) | seL4 Microkit `.system` (#205, nixos-dotfiles #432) |
|---|---|---|
| compartment, `origin: derived`, native Rust | one cloud-hypervisor process per compartment: per-VM uid, seccomp and Landlock jail, one process per device. `--kernel` is the UKI and `--disk` the erofs + verity image (nd#422) | `<protection_domain name=… priority=… budget=… period=…>` with `<program_image>`: a rust-sel4 Microkit PD, which needs #205's `no_std` core |
| compartment, Linux or foreign | the same VM shape, with the program under `compartment-init` | a `<protection_domain>` for its libvmm VMM, holding a `<virtual_machine>` with `<vcpu>` entries |
| `resources.vcpus`, `memory_mib` | `--cpus boot=N`, `--memory size=M` (no `shared=on` unless a region needs it) | a `<vcpu id>` per vCPU; guest RAM as a `<memory_region>` mapped into the VM |
| `resources.cores`, `smt_exclusive` | cgroup cpuset pinning of the VMM and its vCPU threads | static core assignment (the multikernel route, report 01 §4) |
| `priority`, `budget_us`, `period_us` | cgroup `cpu.weight` and `cpu.max` | PD `priority`, and MCS `budget`/`period` |
| `memory_regions` | memfd-backed shared memory (ivshmem or vhost-user); same label only, by SC021 | `<memory_region name size page_size>`, plus `<map mr vaddr perms cached>` in each PD |
| channel `rpc` | a vsock stream (#190) through the host vsock router in the compartment manager (nd#424), with TLS and channel binding. The router admits exactly the listed (client, server, channel id) triples | a `<channel>` (a notification; `pp` for protected calls where the server is passive), plus request and response rings. Across labels the rings go through a small copier PD; within one label they are direct |
| channel `ring` | an ivshmem region with a doorbell, or vhost-user | `<memory_region>` maps, plus a `<channel>` as the doorbell |
| channel `notify` | a zero-length frame on a dedicated vsock stream (cloud-hypervisor's vsock is stream-only) | `<channel><end pd id/><end pd id/></channel>` |
| `quota` | the router's per-channel credit, message size cap and queue depth; a cgroup memory limit for the VMM | ring sizes and the server's admission check; donated memory comes out of the client's budget |
| `restrict` (procedures, hosts, rate) | enforced by the server's RPC layer (`MeshPeerGuard`) and by interposed proxies (the egress proxy) | enforced by the server PD or a proxy PD; the kernel does not see it |
| labels, declassifiers | static, by the checker. At run time they hold only through the wiring the checker allowed | static, by the checker. The kernel enforces exactly that wiring |
| `runtime_grants` | `compartment-init` provides `Clock`, `Random` (reseeded per clone) and `Logger` on a log-sink channel | the same, in the PD runtime |
| `devices` | VFIO passthrough into a driver VM behind the IOMMU | a device `<memory_region phys_addr>` and `<irq irq id>` in a driver PD (sDDF) |
| `image.measurement`, `manifest_digest` | on the UKI command line, checked by `compartment-init` | the PD's ELF digest, recorded for the loader |

The Microkit backend lands in 0.15.0 as a **generator with golden tests**, checked by the
Microkit SDK tool where CI has it. Running it on seL4 needs the `no_std` core (#205) and aarch64
hardware (nixos-dotfiles #432, a horizon item). Generating it from day one keeps the description
honest: an element that cannot be mapped to Microkit encodes something the seL4 target could not
enforce.

### 6. Foreign software

The owner rule of 2026-11-27 (nixos-dotfiles #426): policy is derived from types even for software
we did not write, so **we write its `R`**.

- **One type file per foreign program**, in the topology's crate:

  ```rust
  id_effect_topology::foreign_program! {
    name: "forgejo",
    uses: caps!(Postgres, LogSink, Egress<GithubApi>),
    exposes: [HttpGit],
    reviewed_by: ["tom"],
  }
  ```

  It uses the same capability types and descriptors as our own code, and produces the same
  `id-effect.manifest/1` bytes with `"origin": "foreign"`. The manifest is built as a separate
  artefact, and its digest goes on the compartment's command line in the same way.
- **The first draft comes from observation.** A learning-mode run under strace, or a Landlock audit,
  records what the program touches. The type file is then reviewed for minimality, because
  unreviewed manifests over-grant (Android). SC012 rejects a foreign compartment whose
  `reviewed_by` is empty.
- **Enforcement is entirely the boundary and interposed proxies.** A foreign program speaks its own
  protocols, so `compartment-init` and proxy compartments translate: Postgres over a vsock tunnel,
  an egress proxy holding the host allowlist, a portal for user files. Inside the guest, its
  ambient authority is unbounded, which is why it gets nothing the description does not grant.
- **Default labels:** the empty integrity set ("internet"), until the owner raises it (Q11).

### 7. Enforcement points (ADR 0013 form)

| Rule | Statement | Enforcement point (mechanical) | Leaf |
|---|---|---|---|
| S1 | A compartment's manifest is the one compiled into the binary that runs. | integration test `manifest_section_matches_entry_r` over a built example binary; `compartment_init_refuses_manifest_digest_mismatch`; compile-fail `compartment_r_without_descriptor` and `compartment_r_with_duplicate_capability` | #192, #196 |
| S2 | Kind, instance and restriction stay separate. | the manifest schema has no instance or restriction field (`manifest_rejects_unknown_fields`); `route_cannot_change_kind` in `id_effect_topology` | #192 |
| S3 | Every `use` is satisfied. | checker SC010; corpus `reject/sc010_*` | #193 |
| S4 | No unused route. | checker SC011; corpus `reject/sc011_*` | #193 |
| S5 | No route that is not allowed. | checker SC012; corpus `reject/sc012_*` | #193 |
| S6 | No flow from low to high integrity, or from high to low secrecy, except through a declared declassifier. | checker SC020 and SC021; property `label_flow_matches_reference` (§8) | #193, #204 |
| S7 | Quotas are finite and the host is not overcommitted. | checker SC030; corpus `reject/sc030_*` | #193, #203 |
| S8 | Realisers accept only checked descriptions. | backends take `Checked`, which only `check` constructs (compile-fail `checked_cannot_be_constructed`); `realiser_refuses_unchecked_description` in the KVM realiser (nixos-dotfiles #423) | #193, nd#423 |
| S9 | The checker stays small and independent. | `scripts/check-syscheck-budget.sh` (LOC budget); a CI `cargo tree` guard (no `id_effect*` dependency) | #193 |
| S10 | Hidden authority is made visible. | a `DynEnv` at a root sets `dyn_env_root` and records `dangerous_dyn_env_root` (`posture_records_dyn_env_root`); the ambient-std lints | #192, #194 |
| S11 | Foreign manifests are reviewed. | SC012 on an empty `reviewed_by`; one type file per foreign program | nd#426 |

### 8. Test strategy

- **Property tests on the checker.** They use proptest in `id_effect_syscheck/tests`, with the
  checker's own types and an independent generator that exists only for the tests.
  - *Accepts:* random satisfiable systems are accepted. Each has random compartments and manifests,
    a route for each use, and labels drawn consistent with the routes.
  - *Targeted mutations are rejected with the right code:*
    - drop a channel → SC010;
    - add a channel to a use that does not exist → SC011;
    - route a restricted capability to an unlisted consumer → SC012;
    - add a secrecy tag to a source, or remove an integrity tag on a path without a
      declassifier → SC020;
    - map a region into two labels → SC021;
    - push memory past the host's → SC030.
  - *Differential label check:* SC020's edge rule agrees with a brute-force reference that
    enumerates every simple path, with declassifier semantics, on graphs of up to 8 compartments.
  - *Metamorphic:* permuting arrays changes neither the verdict nor the digest, and renaming
    compartments consistently does not change the verdict. Canonical encode → parse → encode is the
    identity.
  - *Declassifier necessity:* removing a declassifier from a path that needed it turns an accept
    into SC020.
- **Fuzzing.** A `cargo-fuzz` target on `check(bytes)` asserts no panics and no unbounded
  allocation (input size is capped), and that every error is a `Violation`.
- **Golden corpus**, in `crates/id_effect_syscheck/tests/corpus/`:
  - `accept/*.json`, each with its expected digest, its expected Microkit `.system`
    (`*.system.golden`) and its expected KVM realiser plan (`*.kvm.golden`, shared with
    nixos-dotfiles #423);
  - `reject/*.json`, each with its expected violation codes;
  - the Warden exit-KPI system (#198), as an accept case.

  The corpus is the contract between this repository and the dotfiles realiser, and CI runs it in
  both.
- **Manifest tests.** CI builds an example compartment binary, extracts its section and compares it
  with a golden manifest. A change to the binary's `R` must change the golden file. A supply-chain
  change to authority therefore shows up as a reviewed diff (Austral's argument, report 03 §2).
- **Simulation.** #198 runs the Warden system under `TestExecutor` with the fake hypervisor (#195),
  so realiser plans are exercised without KVM.

## Consequences

Positive:

- Policy for our own code is generated from the binary that ships, so it cannot drift. New
  authority shows up as a manifest diff in review.
- The trusted part of the pipeline is the checker and the realisers. They are small, pure and
  independent of the generator. Voltaic and the topology library can be wrong without a realiser
  acting on a bad description.
- One description serves both backends, so moving to seL4 is a retarget.
- Label and quota rules are global properties of one artefact, checked before anything boots,
  instead of per-host firewall rules.

Negative:

- Every cross-compartment capability needs a descriptor and a committed contract snapshot. That is
  authoring work, and descriptor names become an API that has to be versioned across the fleet.
- The checker duplicates the schema types on purpose. A schema change touches three places: the
  generator, the checker and the corpus.
- Strict versioning (unknown fields rejected, no minor versions) makes every new field a new
  format major, and both realisers need a checker update to accept it.
- Labels at compartment granularity are coarse. A compartment that really handles two labels has
  to be split, or run as one instance per label (Flume), which costs RAM.
- `R` proposes and the boundary disposes. Inside a guest, `unsafe`, FFI and std still reach
  whatever the guest kernel allows. That holds until #194 lands, and for foreign software it holds
  for good.
- On KVM, labels and restrictions are enforced by the checker and by user-space servers and proxies
  on top of a large host kernel. Only the seL4 backend puts the wiring under a verified kernel.

## Alternatives considered

- **Hand-written manifests for our own code, next to the source (Fuchsia CML).** Rejected: they
  drift, and the Android permissions study found about a third of apps over-privileged (report 03
  §1.1). Hand-written manifests are kept only for foreign software, which has no `R`.
- **Extract the manifest from source,** with a proc-macro or a rust-analyzer pass. Rejected: macros
  do not see resolved types, and a source-level answer describes a different artefact from the one
  that boots (§1.2).
- **No flat artefact: realisers read the topology directly.** Rejected: the realiser would then
  trust the whole Voltaic and topology stack. capDL's lesson is to check one flat artefact (report
  03 §1.1, option G).
- **Share schema types between the generator and the checker.** Rejected for independence: a
  shared bug would make both agree on a wrong description.
- **Use capDL itself, or Microkit `.system`, as the one description.** Rejected. capDL works at the
  level of seL4 objects (CNodes, slots), too low for the KVM realiser, and Microkit has no labels,
  quotas or manifest digests. Both are *backends* of ours.
- **Qubes-style policy strings** (`service +arg source target allow`). Rejected: they merge kind,
  instance and restriction, and they name services by string (report 03 §1.2, §3.2).
- **Fetch policy at run time (Ghaf).** Rejected: policy must be a build output, with the system's
  provenance.
- **Lattice labels (a total order per dimension) instead of tag sets.** Rejected as the default.
  Tag sets compose across independent secrets (`deploy-keys` and `trading` are incomparable), and a
  total order is a special case of them (Q4).

## Prior art

These come from nixos-dotfiles `docs/research/compartments/03-prior-art-policy.md`:

- **Singularity:** sealed processes, typed channel contracts written as state machines, and
  manifests generated from code and checked at install time. It is the nearest ancestor of the
  whole plan (§2, "Singularity channel contracts"; Fähndrich et al., EuroSys 2006).
- **Fuchsia Component Framework / CML:** use/offer/expose, typed by protocol name, with build-time
  route verification (`ffx scrutiny verify routes`). Security-policy allowlists for sensitive
  capabilities are kept separate from routing. Its weakness is manifests written by hand beside the
  code (§1.1, §6.1).
- **Genode:**
  - session routing by the parent, in recursive realms;
  - quota donation, where the client pays for the server's session state;
  - session labels that each parent on the path prepends, so the child cannot forge them (§1.1,
    §3.2 lesson 4).
- **seL4 CAmkES / capDL / Microkit:** an architecture description is compiled to a flat capability
  distribution, and an initialiser provably establishes it. Microkit simplifies this to protection
  domains, channels and memory regions (§1.1). Cogent's certifying compilation is the model for
  "check the output, don't prove the generator" (§2).
- **Also used:**
  - Flume/HiStar labels at process granularity (§2, §5.2);
  - MILS guards as the only downgrade points (§1.1);
  - Kata/CoCo genpolicy, which generates policy from a manifest the workload already has (§1.1);
  - WIT worlds as residual imports (§2).

## Open questions (with defaults)

| # | Question | Default |
|---|---|---|
| Q1 | Encoding: canonical JSON, CBOR or KDL? | Canonical JSON, by the rules of `id-effect.rpc.contract/1`: one canonicaliser in the ecosystem, and diffs people can read. |
| Q2 | The checker's JSON parser: `serde_json`, or a hand-written one inside the budget? | `serde_json` with `deny_unknown_fields`. Revisit if an audit wants the dependency gone; a hand-written parser must still fit the 1,500-line budget. |
| Q3 | Manifest sections on Mach-O and PE? | No. ELF and wasm custom sections only. Other targets get `Manifest::to_json()` for development. |
| Q4 | Labels as tag sets, or as a total order per dimension? | Tag sets (Flume). |
| Q5 | One-way `rpc` contracts (fire-and-forget), to relax SC020's two-way rule? | Not in `/1`: every `rpc` channel is two-way. A one-way kind would be a new format major. |
| Q6 | Where is `policy.restricted` authored? | In a separate Voltaic module whose code owner is the operator, not inline with the routes. |
| Q7 | Nested realms in the flat format? | No. Lowering flattens them, and names are path-qualified. |
| Q8 | Should the checker also validate the description against the topology (full translation validation)? | Not in 0.15.0. It checks global properties, and manifest digests tie the description to the binaries. Revisit with #204. |
| Q9 | Who owns the capability descriptor namespace? | `package` is the owning crate's name, and the major is bumped by the rpc `check` rules (#189). There is no central registry. |
| Q10 | May `notify` channels cross labels, given each is a one-bit covert channel? | Yes, and they are listed in the threat model. A `policy` switch that forbids cross-label `notify` can come later. |
| Q11 | Default labels for foreign compartments? | Empty integrity ("internet") and secrecy as declared, until the owner raises them. |
| Q12 | Does the checker also run inside `compartment-init`? | Not in 0.15.0. The guest checks only its own manifest digest, and the host realiser is the gate. |

## References

- id_effect issues: #179 (0.15.0 epic), #192, #193, #194, #196, #198, #203, #204, #205; ADR 0018
  (#187), ADR 0019 (#188), contract identity (#189), the vsock transport (#190),
  `CompartmentSupervisor` (#195).
- nixos-dotfiles issues: #417 (Voltaic IaC), #419 (compartments epic), #422 (image generator), #423
  (KVM realiser), #424 (compartment manager), #426 (foreign capability types), #432 (seL4 horizon).
- Research, in nixos-dotfiles `docs/research/compartments/`:
  - `00-synthesis.md`;
  - `01-tcb-substrates.md` (§2.1, §5);
  - `03-prior-art-policy.md` (§0, §1, §2, §4, §5, §6);
  - `04-build-attest-mesh.md` (§0, §2.5, §4.3, §4.4, §6).
- ADRs 0002–0005 and 0011 (capabilities and typed `R`), 0009 (process mesh), 0013 (secure by
  default), 0017 (portable by default).
