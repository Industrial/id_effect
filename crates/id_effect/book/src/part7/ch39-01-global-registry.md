# Cluster-wide names: `GlobalRegistry`

`ProcessNode::register` names a process on one node. `Mesh::global()` names it for the whole
cluster, like BEAM's `global`: any node can resolve the name and reach the process.

```rust,ignore
let pid = b.spawn_local("tally", &args, None)?;
b.global().register("tally", &pid)?;           // claimed by the hosting node
a.global().whereis("tally");                    // Some(pid), from any node
a.global().cast("tally", add(1))?;              // route by name
a.global().call("tally", get, timeout);         // Effect<Vec<u8>, RemoteFault, R>
```

| Operation | Behaviour |
|---|---|
| `register(name, &pid)` | The pid must live on this node. Fails with `GlobalNameError::AlreadyRegistered` when this node's replica already knows the name, `NoProc` for a dead pid, `InvalidName` for an empty name or one over `MAX_NAME_LEN` (255) bytes. |
| `register_addr(name, &addr)` | Same, and keeps the typed `Addr<P>` so `whereis_addr::<P>(name)` returns it on this node. Other nodes resolve the pid and call through the mesh. |
| `whereis(name)` | Answers from the local replica; no network round trip. |
| `unregister(name)` | Only the owner node releases a name (`Ok(true)`); `Ok(false)` when it is free, `NotLocal` when another node owns it. |
| `cast` / `call` | Resolve the name, then `Mesh::remote_cast` / `Mesh::remote_call`; `RemoteFault::NoProc` when the name is free. |
| `sync_with(node)` | Send this node's claims to `node`. |

## How names stay right

Every node keeps a full replica of the name table. The node hosting the process owns the
claim: it broadcasts the registration, monitors the process, and broadcasts the release when
the process exits. `Mesh::peer_down(node)` drops every claim that node owned, so a name never
outlives its node. `Mesh::add_peer` sends the new peer the claims this node owns, and a sync
replaces everything the receiver held for the sender — a register or release lost on the wire
(delivery is at-most-once) is repaired at the next sync.

A node accepts a claim only from the node that owns it, for a pid on that node, and never
about itself.

## Partitions and conflicts

Registration checks the local replica, so during a partition each side can claim the same
name. When the partition heals, both sides sync (`add_peer` on reconnect, or `sync_with` on
each side) and every node applies the same rule:

1. a claim from the node that already owns the name replaces it (the owner's latest word);
2. otherwise the claim whose owner has the lowest `(incarnation, node name)` wins. Node
   incarnations are ordered by boot time, so the node that has been up longest keeps the name;
3. remaining ties fall to the lowest pid.

The losing node drops its claim and sends the losing process an exit signal whose reason is
`name_conflict(name)` (`ExitReason::NameConflict(name)`; `conflicting_name(&reason)`
recognises it). It is an ordinary trappable exit: a process that traps exits keeps running
without the name, and a supervisor sees an abnormal exit.

The registry is available rather than consistent: in a partition, `whereis` on each side
answers with that side's owner. Use a `ClusterSingleton` with a CP lease when two owners must
never exist at once.

**Runnable example:** `id_effect_node/examples/135_global_names.rs`. Tests:
`crates/id_effect_node/tests/global_registry.rs`.
