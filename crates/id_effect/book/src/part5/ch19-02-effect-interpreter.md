# Effect interpreter

`Interpreter` registers action factories keyed by `(state, event)`. Each factory returns a fresh `Effect` (effects are not `Clone`). `Interpreter::step` and `Interpreter::run` take the machine **by value** and return an `Effect` that yields the updated machine — nothing runs until you interpret it, and no step nests a blocking interpreter inside another effect.

```rust
use id_effect::{Effect, run_blocking};
use id_effect_fsm::{Interpreter, StateMachine, TransitionTable};

let table = TransitionTable::new()
    .on("idle", "tick", "busy")
    .on("busy", "tick", "idle");

// Actions see the effect environment — here a tick counter.
let interp = Interpreter::<&str, &str, (), String, u32>::new().on_transition("idle", "tick", || {
    Effect::new(|ticks: &mut u32| {
        *ticks += 1;
        Ok(())
    })
});

let program = interp.run(StateMachine::new("idle", table), ["tick", "tick"]);
let machine = run_blocking(program, 0).expect("valid events");
assert_eq!(machine.state(), "idle");
```

Semantics of one step:

1. The table is consulted first. A missing edge fails with `RunError::Transition` and runs no action.
2. The registered action (if any) runs with the caller's environment.
3. The new state is committed only after the action succeeds; an action failure is `RunError::Effect(err)`.

Interpret at the application boundary (`run_blocking`, `run_async`, a fiber runtime) — or host the machine in a process, as the next section shows.

## `FsmProcess`: a `gen_statem`-style behaviour

`FsmProcess` implements `id_effect_process::Process`. Events are mailbox messages, transition actions run inside the process with the process environment, and the state is queryable:

```rust,no_run
use std::time::Duration;
use id_effect::run_blocking;
use id_effect_fsm::{Fsm, FsmProcess, FsmSpec};
use id_effect_process::{NodeName, ProcessNode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Ev { Coin, Push }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Fsm)]
#[fsm(event = Ev)]
enum Turnstile {
    #[fsm(on(Coin => Unlocked))]
    Locked,
    #[fsm(on(Push => Locked))]
    Unlocked,
}

let node = ProcessNode::threads(NodeName::new("gate@local"));
let gate = FsmProcess::start(&node, FsmSpec::<Turnstile, Ev, String, ()>::from_fsm(), ());
let t = Duration::from_secs(1);
assert_eq!(run_blocking(gate.fire(Ev::Coin, t), ()), Ok(Ok(Turnstile::Unlocked)));
run_blocking(gate.cast(Ev::Push), ()).unwrap();
assert_eq!(run_blocking(gate.state(t), ()), Ok(Turnstile::Locked));
```

(`no_run`: spawns OS threads for the process.)

- `fire` (call) replies with the new state, or the `FsmError` when no edge exists — the process keeps running.
- `cast` / `Addr::send` apply events in mailbox order; events without an edge follow `Unhandled::Ignore` (default) or `Unhandled::Stop`.
- A failing transition action crashes the process with `ExitReason::Error`; under a supervisor it restarts from its `FsmSpec` (see the `supervised_traffic_light` example).
