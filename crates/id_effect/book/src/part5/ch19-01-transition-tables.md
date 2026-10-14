# Transition tables

Build tables fluently, then freeze them into a `StateMachine`:

```rust
use id_effect_fsm::{StateMachine, TransitionTable};

let table = TransitionTable::new()
    .on("idle", "start", "running")
    .on("running", "stop", "idle");

let mut m = StateMachine::new("idle", table);
assert_eq!(m.step("start"), Ok("running"));
assert!(m.step("start").is_err()); // no edge running --start-->
```

Missing edges return `FsmError::NoTransition { state, event }` and leave the state unchanged. Reset with `StateMachine::reset`, or set the state directly after loading a durable snapshot.

## `#[derive(Fsm)]`

For enum states, declare the edges where the states are defined. The derive (re-exported by `id_effect_fsm`) implements the `Fsm` trait: `initial()`, `transition_table()`, and a provided `state_machine()`.

```rust
use id_effect_fsm::Fsm;

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

let mut door = Door::state_machine();
assert_eq!(door.step(DoorEvent::Lock), Ok(Door::Locked));
assert!(door.step(DoorEvent::Open).is_err());
```

Syntax:

- On the enum: `#[fsm(event = EventType, initial = Variant, crate = path)]`. `event` is required; `initial` defaults to the first variant; `crate` overrides the `::id_effect_fsm` path for crates that re-export it.
- On a source state: `#[fsm(on(Event => Target, ..))]`, repeatable. A bare `Event` means `EventType::Event`; a multi-segment path is used as written.

Unknown targets, an unknown `initial`, data-carrying variants, generic enums, and duplicate `(state, event)` edges are compile errors.
