# Transition tables

Build tables fluently, then freeze them into a [`StateMachine`](../../id_effect_fsm/src/machine.rs):

```rust
use id_effect_fsm::{StateMachine, TransitionTable};

let table = TransitionTable::new()
    .on("idle", "start", "running")
    .on("running", "stop", "idle");

let mut m = StateMachine::new("idle", table);
m.step("start")?;
```

Missing edges return [`FsmError::NoTransition`](../../id_effect_fsm/src/error.rs). Reset with [`StateMachine::reset`](../../id_effect_fsm/src/machine.rs) or set state directly after loading a durable snapshot.
