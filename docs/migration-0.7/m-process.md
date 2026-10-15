# m-process: `id_effect_process` on the 0.7 kernel

Every process is now a fiber on an `id_effect::next::Runtime`, and every behaviour callback returns
an `id_effect::next::Effect`. The 0.6 thread-per-process node, the busy-polling `SpinClock`, and
the `run_mailbox_op` workaround (mailbox and cell effects interpreted in place with
`run_blocking`) are gone. Time comes from the runtime's executor clock, so a node on a
`TestExecutor` runs call timeouts, shutdown deadlines and restart-intensity windows on virtual
time.

## What changed

| 0.6 | 0.7 |
|---|---|
| `ProcessNode::threads(name)` | `ProcessNode::new(name)` (fibers on `Runtime::global()`) |
| `ProcessNode::on_runtime(name, v1_runtime)` | `ProcessNode::on_runtime(name, next::Runtime)` / `ProcessNode::on_executor(name, Arc<dyn Executor>)` |
| `ProcessNode::with_spawner(name, clock, spawner)`, `Job`, `Spawner` | removed (use `on_executor`, e.g. a `TestExecutor`) |
| `SpinClock`, `ProcessNode::clock()` | removed; `ProcessNode::now()` is the executor clock, `node.runtime()` the runtime |
| `Process` callbacks return v1 `Effect<_, _, Env>`; `type Env: Send` | `next::Effect<_, _, Env>`; `Process: Send`, `type Env: Send + Sync` (provided once at spawn, `HNil` or any value) |
| a panic in a callback was caught with `catch_unwind` | it reaches the loop as a `Defect` (`Cause::Die`); the exit reason is still `Error("panic: <message>")` |
| `Addr::{call, cast, send}` → `Effect<_, _, ()>` | generic over the environment `R` (`Effect<_, _, R>`); run at the edge with `block_on` / `Runtime::run` |
| `Addr::clone_mailbox()` → `Queue<ProcMsg<P>>` | → `Mailbox<ProcMsg<P>>` (`offer_now`, `close`, `take`) |
| `ProcMsg::Call(_, v1 Deferred)`, `ProcessCell::exited()` → v1 `Deferred` | `next::Deferred` |
| `ProcessCell::cancellation()` | removed (kills stay cooperative: reason recorded, mailbox closed) |
| `SupervisorHandle::stop(..)` was an `async fn` | returns an effect (`Effect<(), E, R>`) |
| supervisor `Env = ()` | `SupervisorProcess` / `DynamicSupervisor` use `Env = HNil` |
| — | `ProcessNode::await_exit(&pid)`: waits until the process is fully finalized |

The exit-signal, link, monitor, registry and supervision semantics (ADR 0009) are unchanged.
Supervisor restarts stay immediate (OTP): the budget is the sliding `RestartIntensity` window,
not a backoff delay.

## Before / after

```rust
// 0.6
use id_effect::{Effect, run_blocking, succeed};
let node = ProcessNode::threads(NodeName::new("app@local"));
let tree = SupervisorSpec::new(Strategy::OneForOne).child(spec).start(&node);
let kids = run_blocking(tree.which_children(Duration::from_secs(5)), ())?;
```

```rust
// 0.7
use id_effect::next::{Effect, HNil, succeed};
let node = ProcessNode::new(NodeName::new("app@local"));
let tree = SupervisorSpec::new(Strategy::OneForOne).child(spec).start(&node);
let kids = tree.which_children::<HNil>(Duration::from_secs(5)).block_on().into_result()?;

// Deterministic test: virtual time.
let exec = TestExecutor::new();
let node = ProcessNode::on_executor(NodeName::new("t@test"), Arc::new(exec.clone()));
exec.run_until_stalled();
exec.advance(Duration::from_secs(5)); // fires shutdown deadlines, slides intensity windows
```

## Dependents

- `id_effect_fsm`: `FsmProcess` now has `Args = (FsmSpec, R)` and `Env = HNil` (it owns the
  actions' v1 env `R` and runs a v1 transition action on the executor's blocking pool until
  `m-fsm`). `FsmProcess::start(node, spec, env)` is unchanged; a raw spawn is
  `spawn_linked_to::<FsmProcess<..>>(sup, (spec, env), HNil)`. `FsmHandle::{state, fire, cast}`
  return `next` effects.
- `id_effect_node`: `BehaviorRegistry::adopt_local` takes a `Mailbox`; wire routes enqueue with
  `offer_now` and bridge the `next` reply into the v1 wire reply.
