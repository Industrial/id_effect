# m-fsm: `id_effect_fsm` on the 0.7 kernel

Transition actions, `Interpreter::{step, run}`, and `Saga::{run, compensate_indices}` now use
`id_effect::next::Effect`. The environment `R` is the next kernel's typed environment (an HList
such as `HCons<Db, HNil>`, or `HNil`) read with `need::<T>()`, not a `&mut R` value. Steps and
compensations are sequenced with `flat_map` / `catch` on the caller's fiber. Nothing in the
crate parks a thread: there is no `run_blocking` in the interpreter or the saga, and the
`FsmProcess` blocking-pool adapter (`on_blocking_pool`) from m-process is gone.

The pure parts (`TransitionTable`, `StateMachine`, `Fsm` / `#[derive(Fsm)]`, `to_mermaid`, the
matchers, `FsmError`, `SagaError`) are unchanged.

## What changed

| 0.6 | 0.7 |
|---|---|
| actions / saga steps: v1 `Effect<A, Err, R>` over `&mut R` | `next::Effect<A, Err, R>`, `R: Send + Sync` (an HList; read capabilities with `need::<T>()`) |
| `Interpreter::step` / `run` → v1 `Effect<StateMachine, RunError, R>` | → `next::Effect<StateMachine, RunError, R>`; `S`, `E`, `A`, `Err` must be `Send` (`S`, `E` also `Sync`) |
| `Saga::run` / `compensate_indices` → v1 `Effect<(), SagaError<Err>, R>` | → `next::Effect<(), SagaError<Err>, R>`. Only typed failures are compensated; a defect or an interrupt passes through without compensation (as in 0.6, where a panic was not compensated either) |
| run with `run_blocking(program, env)` | `program.provide(..)` / `.provide_env(..)`, then run at the edge (`block_on`, `Runtime::run`) or compose it into the caller's effect |
| `FsmProcess`: `Args = (FsmSpec, R)`, `Env = HNil`; the process owned `R` and ran each action on the executor's blocking pool | `Args = FsmSpec`, `Env = R`: the actions' environment is the process environment, provided once at spawn, and an action runs on the process fiber (it can suspend, e.g. `sleep`) |
| raw spawn: `spawn_linked_to::<FsmProcess<..>>(sup, (spec, env), HNil)` | `spawn_linked_to::<FsmProcess<..>>(sup, spec, env)`; `ChildSpec::worker::<FsmProcess<..>, _>(id, move \|\| (spec.clone(), env.clone()))` |
| `FsmProcess::start(node, spec, env)` / `start_linked(ctx, spec, env)` | unchanged signature; `env: R` with `R: Send + Sync` |
| `FsmHandle<S, E, Err, R>` with `R: Send` | `R: Send + Sync` (its effects were already `next` effects) |

## Before / after

```rust
// 0.6
use id_effect::{Effect, run_blocking};
let interp = Interpreter::<Door, DoorEvent, (), String, u32>::new()
  .on_transition(Door::Closed, DoorEvent::Lock, || {
    Effect::new(|locks: &mut u32| {
      *locks += 1;
      Ok(())
    })
  });
let door = run_blocking(interp.run(Door::state_machine(), [DoorEvent::Lock]), 0)?;
```

```rust
// 0.7
use std::sync::atomic::{AtomicU32, Ordering};
use id_effect::next::{HCons, HNil, need};
let interp = Interpreter::<Door, DoorEvent, (), String, HCons<AtomicU32, HNil>>::new()
  .on_transition(Door::Closed, DoorEvent::Lock, || {
    need::<AtomicU32>().map(|locks| {
      locks.fetch_add(1, Ordering::SeqCst);
    })
  });
let door = interp
  .run(Door::state_machine(), [DoorEvent::Lock])
  .provide(AtomicU32::new(0))
  .block_on() // program edge only
  .into_result()?;
```

```rust
// 0.6: a v1 action env owned by the process
FsmProcess::start(&node, spec, audit_log);            // R = Arc<Mutex<Vec<_>>>
node.spawn_linked_to::<FsmProcess<S, E, Err, R>>(sup, (spec, env), HNil);

// 0.7: R is the process Env
FsmProcess::start(&node, spec, HCons(audit_log, HNil)); // R = HCons<Mutex<Vec<_>>, HNil>
node.spawn_linked_to::<FsmProcess<S, E, Err, R>>(sup, spec, env);
```

## Dependents

- `labs/id_effect_workflow`: uses only the pure `StateMachine` / `TransitionTable` / `FsmError`
  (durable snapshots), so it needs no adapter.
