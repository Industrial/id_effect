# Clusters: groups, singletons, distributed supervision

**Process groups** are an ORSWOT CRDT (`ProcessGroups`) — AP by default;
duplicate/gap windows under partition, merge on heal.

**ClusterSingleton** is CP: at most one holder via a lease backend. `MemoryLease`
is the in-process test backend; [`PgAdvisoryLease`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_node/src/singleton.rs)
(crate feature `pg`) is the production path, holding a Postgres session-scoped
advisory lock on a dedicated connection (a daemon fiber, interrupted when the handle drops)
so the lock releases with the session. `PgAdvisoryLease::connect(url)` and the
`ClusterSingleton` operations (`try_become`, `heartbeat`, `resign`, `is_holder`) are effects;
the pg lease needs a tokio runtime with its IO driver (`Runtime::global()` has one).

**DistributedSupervisor** re-places children on `NodeDown` (Horde-style
takeover) using the behavior registry + mesh placement. It honors each child's
`prefer` hint and excludes downed nodes when choosing a takeover target. `start_child`,
`on_membership` and `retry_orphaned` return effects; on `NodeUp` / `Rejoined` the orphan
retry runs as a daemon fiber (`RetryHandle` is a `FiberHandle`).

Fabric placement routes every decision through the single [`place`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_node/src/placement.rs)
scorer, fed by [`LoadReport`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_node/src/placement.rs) telemetry and bounded by
`ClusterResourcePolicy` (all in `id_effect_node::placement` since 0.6). `id_effect_node::scale_out_placed` records the chosen `target_node` so the
job runner dispatches work to the right node.

**Runnable examples:**
`id_effect_node/examples/132_distributed_supervisor_failover.rs` (takeover on
`NodeDown`) and `id_effect_node/examples/133_cluster_stream_fanout.rs`
(`fanout_cast` to a process group across nodes).
