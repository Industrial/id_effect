# m-node: `id_effect_node` on the 0.7 kernel

Everything in the distribution layer that waits is now an `id_effect::next::Effect`: `Send`,
interruptible, and timed on the executor clock. The 0.6 sleep-polling waits
(`Mesh::await_deferred` looped on `run_blocking(reply.poll())` + `thread::sleep(2ms)`), the
thread per inbound call that relayed the reply, the thread per orphan retry, and the Postgres
lease's worker thread with its own tokio runtime are gone: they are fibers now.

## What changed

| 0.6 | 0.7 |
|---|---|
| `Mesh::remote_spawn(..) -> Result<Pid, RemoteFault>` (parks the caller) | `-> Effect<Pid, RemoteFault, R>`: sends when it runs; `timeout` is on the calling fiber's executor clock; interruptible, never leaves a pending entry |
| `Mesh::remote_call(..) -> Result<Vec<u8>, RemoteFault>` | `-> Effect<Vec<u8>, RemoteFault, R>` (same contract) |
| `Mesh::remote_spawn_placed(..) -> Result<Pid, RemoteFault>` | `-> Effect<Pid, RemoteFault, R>` |
| an inbound `Call` was answered from a spawned OS thread | answered by a daemon fiber on the local `ProcessNode`'s runtime |
| `BehaviorRegistry::deliver_call(pid, bytes, v1 Deferred)` | `deliver_call::<R>(pid, bytes) -> Result<Effect<Vec<u8>, RemoteFault, R>, RemoteFault>` (enqueued now, the effect awaits the encoded reply); `CallWireFn` returns a `ReplyFuture` |
| `DistributedSupervisor::start_child(..) -> Result<Pid, RemoteFault>` | `-> Effect<Pid, RemoteFault, R>` (candidates tried in order, each spawn bounded) |
| `DistributedSupervisor::on_membership(&e) -> Option<RetryHandle>` | `-> Effect<Option<RetryHandle>, Never, R>`: `NodeDown` completes once the takeover is done; `NodeUp`/`Rejoined` fork the orphan retry as a daemon fiber |
| `RetryHandle = std::thread::JoinHandle<Vec<(String, RemoteFault)>>` | `RetryHandle = next::FiberHandle<Vec<(String, RemoteFault)>, Never>` (`join()` / `interrupt_now()`; an interrupted retry leaves its children orphaned and retryable) |
| `DistributedSupervisor::retry_orphaned() -> Vec<..>` | `-> Effect<Vec<..>, Never, R>` |
| `SingletonLease` methods return `Result<_, SingletonError>` / `Option<String>` | return `Effect<(), SingletonError>` / `Effect<Option<String>, Never>` |
| `ClusterSingleton::{try_become, heartbeat, resign}` → `Result`, `is_holder()` → `bool` | → `Effect<(), SingletonError>`, `is_holder()` → `Effect<bool, Never>` (`local_pid()` stays sync) |
| `PgAdvisoryLease::connect(url)` / `from_pool_config(cfg)` blocked on a worker thread | → `Effect<PgAdvisoryLease, SingletonError>`; the dedicated session is a daemon fiber, interrupted when the handle drops. Run it on a `TokioExecutor` over a tokio runtime with the IO driver (sqlx needs it; `Runtime::new()` / `Runtime::global()` have it). Elsewhere the connect fails with `Unavailable("lease worker exited before ready: …")` naming the session fiber's defect |

Unchanged: `remote_cast` / `remote_info` / `remote_monitor` / `remote_demonitor` / `peer_down`
(synchronous, at-most-once), `fanout_cast`, transports, sessions, the handshake, SWIM membership,
process groups, placement and the `control` DTOs. Wire protocol and semantics (ADR 0009) are
unchanged.

## Before / after

```rust
// 0.6
let pid = mesh.remote_spawn("b@host", "counter", args, None, Duration::from_secs(2))?;
let reply = mesh.remote_call(&pid, msg, Duration::from_secs(2))?;
sup.on_membership(&MembershipEvent::NodeDown(member));
lease.try_acquire("leader", "a", ttl)?;
```

```rust
// 0.7: compose inside a program …
use id_effect::next::Effect;
let program: Effect<Vec<u8>, RemoteFault> = mesh
  .remote_spawn("b@host", "counter", args, None, Duration::from_secs(2))
  .flat_map(move |pid| mesh2.remote_call(&pid, msg, Duration::from_secs(2)));
// … or run at the edge.
let reply = program.block_on().into_result()?;
let _ = sup.on_membership(&MembershipEvent::NodeDown(member)).block_on();
// The pg lease needs tokio's IO driver (the default runtime has it).
let lease = PgAdvisoryLease::connect(url).block_on().into_result()?;
lease.try_acquire("leader", "a", ttl).block_on().into_result()?;
```

## Dependents

`id_effect_axum` (`cluster` feature: `ClusterStatus`, `DrainRequest`, `RebalanceRequest`,
`MemberView`, `ProcessView`) and `labs/id_effect_jobs` (`placement::scale_out_placed`) use only
the unchanged DTO / placement API: no adapters needed.
