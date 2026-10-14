# `id_effect_fsm`

Typed **finite-state machines** for [`id_effect`](https://github.com/Industrial/id_effect)
programs: pure transition tables, `#[derive(Fsm)]`, Effect-valued transition actions, sagas with
compensation, and a `gen_statem`-style process behaviour (`FsmProcess`) for supervised machines.

## Modules

| Item | Role |
|------|------|
| `TransitionTable` / `StateMachine` | Pure edge map + `step` |
| `Fsm` / `#[derive(Fsm)]` | Declare initial state and edges on a fieldless enum |
| `Interpreter` | Effect-valued transition actions; `step` / `run` return an `Effect` |
| `FsmProcess` / `FsmSpec` / `FsmHandle` | Host a machine in an `id_effect_process` process: events are mailbox messages, state is queryable, crashes are supervised |
| `Saga` / `SagaStep` | Forward steps with LIFO compensation, run as one `Effect` |
| `to_mermaid` / `table_to_mermaid` | Mermaid `stateDiagram-v2` export |
| `TaggedEvent` / `event_matcher` | Bridge events/states to `HasTag` / `Matcher` |

Durable FSM snapshots live in the unpublished labs crate `id_effect_workflow::fsm`.

## Quick example

```rust
use id_effect::{Effect, run_blocking};
use id_effect_fsm::{Fsm, Interpreter};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DoorEvent { Open, Close, Lock, Unlock }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Fsm)]
#[fsm(event = DoorEvent, initial = Closed)]
enum Door {
  #[fsm(on(Open => Opened, Lock => Locked))]
  Closed,
  #[fsm(on(Close => Closed))]
  Opened,
  #[fsm(on(Unlock => Closed))]
  Locked,
}

// Effect-valued action on Closed --Lock--> Locked; the environment is a lock counter.
let interp = Interpreter::<Door, DoorEvent, (), String, u32>::new()
  .on_transition(Door::Closed, DoorEvent::Lock, || {
    Effect::new(|locks: &mut u32| {
      *locks += 1;
      Ok(())
    })
  });

let program = interp.run(Door::state_machine(), [DoorEvent::Lock, DoorEvent::Unlock]);
let door = run_blocking(program, 0).expect("valid events");
assert_eq!(door.state(), Door::Closed);
```

`#[fsm(event = E, initial = Variant)]` goes on the enum (`initial` defaults to the first
variant); `#[fsm(on(Event => Target, ..))]` on each source state. Unknown targets, duplicate
edges, and data-carrying variants are compile errors.

## As a supervised process

```rust,no_run
# use id_effect_fsm::Fsm;
# #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
# enum DoorEvent { Open, Close }
# #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Fsm)]
# #[fsm(event = DoorEvent)]
# enum Door { #[fsm(on(Open => Opened))] Closed, #[fsm(on(Close => Closed))] Opened }
use std::time::Duration;
use id_effect::run_blocking;
use id_effect_fsm::{FsmProcess, FsmSpec};
use id_effect_process::{NodeName, ProcessNode};

let node = ProcessNode::threads(NodeName::new("door@local"));
let door = FsmProcess::start(&node, FsmSpec::<Door, DoorEvent, String, ()>::from_fsm(), ());
run_blocking(door.cast(DoorEvent::Open), ()).unwrap();
let state = run_blocking(door.state(Duration::from_secs(1)), ()).unwrap();
assert_eq!(state, Door::Opened);
```

## Examples and book

```bash
cargo run -p id_effect_fsm --example door_fsm
cargo run -p id_effect_fsm --example supervised_traffic_light
```

mdBook **Part V — ch19 State Machines** (`crates/id_effect/book/src/part5/`).
