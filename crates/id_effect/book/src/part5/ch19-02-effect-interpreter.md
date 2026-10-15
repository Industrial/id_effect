# Effect interpreter

`Interpreter` registers action factories keyed by `(state, event)`. Each factory returns a fresh `Effect` (effects are one-shot, not `Clone`). `Interpreter::step` and `Interpreter::run` take the machine **by value** and return an `Effect` that yields the updated machine: nothing runs until you run it, and the interpreter never blocks a thread.

The actions' environment `R` is the typed environment of Part II (an HList such as `HCons<Db, HNil>`, or `HNil`); actions read capabilities with `need::<T>()`.

```rust
use std::sync::atomic::{AtomicU32, Ordering};
use id_effect::{HCons, HNil, need};
use id_effect_fsm::{Interpreter, StateMachine, TransitionTable};

let table = TransitionTable::new()
    .on("idle", "tick", "busy")
    .on("busy", "tick", "idle");

// Actions read the environment: here a tick counter capability.
type Ticks = HCons<AtomicU32, HNil>;
let interp = Interpreter::<&str, &str, (), String, Ticks>::new().on_transition("idle", "tick", || {
    need::<AtomicU32>().map(|ticks| {
        ticks.fetch_add(1, Ordering::SeqCst);
    })
});

let machine = interp
    .run(StateMachine::new("idle", table), ["tick", "tick"])
    .provide(AtomicU32::new(0))
    .block_on() // program edge only
    .into_result()
    .expect("valid events");
assert_eq!(machine.state(), "idle");
```

Semantics of one step:

1. The table is consulted first. A missing edge fails with `RunError::Transition` and runs no action.
2. The registered action (if any) runs on the caller's fiber with the caller's environment.
3. The new state is committed only after the action succeeds; an action failure is `RunError::Effect(err)`.

Compose the program into a larger effect, run it at the edge (`block_on`, `Runtime::run(..).await`), or host the machine in a process, as the next section shows.

## `FsmProcess`: a `gen_statem`-style behaviour

`FsmProcess` implements `id_effect_process::Process`. Events are mailbox messages, transition actions run on the process fiber with the process environment (provided once at spawn), and the state is queryable. `FsmHandle::{fire, cast, state}` return effects:

```rust,no_run
use std::time::Duration;
use id_effect::{Cause, Effect, Exit, HNil};
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

/// Runs an effect at the edge (fixes the handle's environment to `HNil`).
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E, HNil>) -> Result<A, E> {
    match eff.block_on() {
        Exit::Success(a) => Ok(a),
        Exit::Failure(Cause::Fail(e)) => Err(e),
        Exit::Failure(_) => panic!("effect died"),
    }
}

let node = ProcessNode::new(NodeName::new("gate@local"));
let gate = FsmProcess::start(&node, FsmSpec::<Turnstile, Ev, String, HNil>::from_fsm(), HNil);
let t = Duration::from_secs(1);
assert_eq!(run(gate.fire(Ev::Coin, t)).unwrap(), Ok(Turnstile::Unlocked));
run(gate.cast(Ev::Push)).unwrap();
assert_eq!(run(gate.state(t)).unwrap(), Turnstile::Locked);
```

(`no_run`: starts a process node on the global runtime.)

- `fire` (call) replies with the new state, or the `FsmError` when no edge exists — the process keeps running.
- `cast` / `Addr::send` apply events in mailbox order; events without an edge follow `Unhandled::Ignore` (default) or `Unhandled::Stop`.
- `FsmSpec::with_interpreter(interp)` attaches transition actions. A failing action crashes the process with `ExitReason::Error`; under a supervisor it restarts from its `FsmSpec` (see the `supervised_traffic_light` example).

Runnable example: `cargo run -p id_effect_fsm --example door_fsm`.
