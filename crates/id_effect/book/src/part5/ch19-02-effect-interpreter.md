# Effect interpreter

[`Interpreter`](../../id_effect_fsm/src/interpreter.rs) registers effect factories keyed by `(state, event)`. Each factory returns a fresh [`Effect`](../../id_effect/src/kernel/effect.rs) (effects are not `Clone`).

```rust
use id_effect::{Effect, run_blocking};
use id_effect_fsm::{Interpreter, StateMachine, TransitionTable};

let interp = Interpreter::new().on_transition("idle", "tick", || {
    Effect::new(|_| Ok(()))
});
let mut m = StateMachine::new("idle", table);
interp.run(&mut m, ["tick"], ())?;
```

Use `run_blocking` at the application boundary — the same rule as other synchronous IO in `effect!` blocks.
