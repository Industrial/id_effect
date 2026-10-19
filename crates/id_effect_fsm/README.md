# `id_effect_fsm`

Typed **finite-state machines** for [`id_effect`](https://github.com/Industrial/id_effect)
programs: pure transition tables, `#[derive(Fsm)]`, Effect-valued transition actions, sagas with
compensation, a process behaviour (`FsmProcess`) for supervised table-driven machines, and
`Statem`, a callback behaviour with `gen_statem` parity.

## Modules

| Item | Role |
|------|------|
| `TransitionTable` / `StateMachine` | Pure edge map + `step` |
| `Fsm` / `#[derive(Fsm)]` | Declare initial state and edges on a fieldless enum |
| `Interpreter` | Effect-valued transition actions; `step` / `run` return an `Effect` |
| `FsmProcess` / `FsmSpec` / `FsmHandle` | Host a machine in an `id_effect_process` process: events are mailbox messages, state is queryable, crashes are supervised |
| `Statem` / `StatemProcess` / `StatemHandle` | `gen_statem` parity as a supervised process: states that carry data, a separate machine `Data`, state, event and generic timeouts, postpone, inserted events, replies and state-enter callbacks; `handle_event` returns a `Transition` with `Action`s |
| `Saga` / `SagaStep` | Forward steps with LIFO compensation, run as one `Effect` |
| `to_mermaid` / `table_to_mermaid` | Mermaid `stateDiagram-v2` export |
| `TaggedEvent` / `event_matcher` | Bridge events/states to `HasTag` / `Matcher` |

For a machine that survives restarts, drive it from a durable workflow of the `id_effect_workflow`
crate (events as signals, each transition a journaled step).

## Quick example

Transition actions and saga steps are `id_effect::Effect`s over a typed environment `R`
(an HList such as `HCons<Locks, HNil>`, or `HNil`); `step` / `run` / `Saga::run` compose them
with `flat_map`, so nothing blocks a thread.

```rust
use std::sync::atomic::{AtomicU32, Ordering};
use id_effect::{HCons, HNil, need};
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
let interp = Interpreter::<Door, DoorEvent, (), String, HCons<AtomicU32, HNil>>::new()
  .on_transition(Door::Closed, DoorEvent::Lock, || {
    need::<AtomicU32>().map(|locks| {
      locks.fetch_add(1, Ordering::SeqCst);
    })
  });

let program = interp
  .run(Door::state_machine(), [DoorEvent::Lock, DoorEvent::Unlock])
  .flat_map(|door| need::<AtomicU32>().map(move |locks| (door.state(), locks.load(Ordering::SeqCst))))
  .provide(AtomicU32::new(0));
// Run at the program edge (`block_on`); inside an effect, compose it instead.
let (state, locks) = program.block_on().into_result().expect("valid events");
assert_eq!((state, locks), (Door::Closed, 1));
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
use id_effect::HNil;
use id_effect_fsm::{FsmProcess, FsmSpec};
use id_effect_process::{NodeName, ProcessNode};

// The handle's effects are `id_effect` effects: run them at the edge with `block_on`.
let node = ProcessNode::new(NodeName::new("door@local"));
// The last argument is the actions' environment `R` (the process `Env`).
let door = FsmProcess::start(&node, FsmSpec::<Door, DoorEvent, String, HNil>::from_fsm(), HNil);
door.cast::<HNil>(DoorEvent::Open).block_on().into_result().unwrap();
let state = door.state::<HNil>(Duration::from_secs(1)).block_on().into_result().unwrap();
assert_eq!(state, Door::Opened);
```

## Examples and book

```bash
cargo run -p id_effect_fsm --example door_fsm
cargo run -p id_effect_fsm --example supervised_traffic_light
cargo run -p id_effect_fsm --example statem_door_lock
```

mdBook **Part V — ch19 State Machines** (`crates/id_effect/book/src/part5/`).
