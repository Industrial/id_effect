# `id_effect_process`

BEAM/OTP-style processes for [`id_effect`](https://github.com/Industrial/id_effect): typed
mailboxes, GenServer-shaped behaviours, links, monitors, `trap_exit`, exit-signal propagation,
name registration, and supervision trees (`OneForOne` / `OneForAll` / `RestForOne`, restart
intensity, `DynamicSupervisor`). Every behaviour callback returns an `Effect`, and timeouts and
restart windows are measured on an injected `Clock`.

This is the local half of the Process Mesh (ADR 0009); `id_effect_node` adds distribution on top.
For a `gen_statem`-style behaviour that hosts a typed state machine, see
`id_effect_fsm::FsmProcess`.

## Concepts

| Item | Role |
|------|------|
| `Process` | Behaviour trait: `init` / `handle_call` / `handle_cast` / `handle_info` / `terminate` |
| `ProcessNode` | Spawns processes; owns the process table, links, monitors, exit signals, registry |
| `Addr<P>` | Typed `call` / `cast` / `send` handle |
| `SupervisorSpec` / `ChildSpec` / `Strategy` | Declarative supervision trees |
| `DynamicSupervisor` | Homogeneous, on-demand children |

## Example: a supervisor with two children

```rust
use std::time::Duration;

use id_effect::runtime::Never;
use id_effect::{Effect, run_blocking, succeed};
use id_effect_process::{
  ChildSpec, Next, NodeName, Process, ProcessCtx, ProcessNode, Strategy, SupervisorSpec,
};

/// A counter that answers `call`s with its current value.
struct Counter(u64);

impl Process for Counter {
  type Args = u64;
  type Env = ();
  type Call = ();
  type Reply = u64;
  type Cast = u64;
  type Info = ();
  type Error = Never;

  fn init(start: u64, _ctx: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Counter(start))
  }

  fn handle_call(self, _: (), _ctx: ProcessCtx) -> Effect<(u64, Next<Self>), Never, ()> {
    let value = self.0;
    succeed((value, Next::Continue(self)))
  }

  fn handle_cast(self, n: u64, _ctx: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    succeed(Next::Continue(Counter(self.0 + n)))
  }
}

let node = ProcessNode::threads(NodeName::new("readme@local"));
let tree = SupervisorSpec::new(Strategy::OneForOne)
  .intensity(3, Duration::from_secs(5))
  .child(ChildSpec::worker::<Counter, _>("hits", || (0, ())))
  .child(ChildSpec::worker::<Counter, _>("misses", || (100, ())))
  .start(&node);

let children = run_blocking(tree.which_children(Duration::from_secs(5)), ()).unwrap();
let ids: Vec<_> = children.iter().map(|(id, _)| id.as_str()).collect();
assert_eq!(ids, ["hits", "misses"]);
```

If `hits` crashes, the `OneForOne` strategy restarts only `hits`; more than three restarts in
five seconds stops the supervisor with `Shutdown`, escalating to its own parent.

## Blocking and async contexts

`Addr::call`, `Addr::cast`, `Addr::send` and the supervisor handles return effects — drive them
with your interpreter of choice (`run_blocking` on plain threads, `run_async` inside an async
runtime). The plain-code entry points (`ProcessNode::spawn`, `link`, `monitor_*`, `exit`,
`Addr::send_now`) never wait on another process: they only touch the node's own tables and
enqueue into an unbounded mailbox, so they are safe to call from inside a current-thread async
runtime. See the `node` module docs for the details.

## Book

mdBook **Part VII — Process Mesh**, especially
[Processes and supervision](../id_effect/book/src/part7/ch37-00-processes-supervision.md).

## Examples and tests

```bash
cargo run -p id_effect_process --example 130_process_tree
cargo nextest run -p id_effect_process
```
