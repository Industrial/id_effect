# state_scan — FSM stepping

[`state_scan`](../../src/streaming/state_scan.rs) folds state while emitting **only when the step returns `Some`**.

Contrast with [`Stream::scan`](../../src/streaming/stream.rs), which emits on every element:

```rust
use id_effect::Stream;

#[derive(Clone, Copy, PartialEq)]
enum Mode { Idle, Active }

let transitions = Stream::from_iterable([0u8, 1, 1, 0, 1]).state_scan(Mode::Idle, |mode, x| {
    let next = if x > 0 { Mode::Active } else { Mode::Idle };
    let emit = matches!((mode, next), (Mode::Idle, Mode::Active));
    (next, emit.then_some(x))
});
// emits on Idle → Active edges only
```

Use this for simple finite-state interpretations without pulling in the full `id_effect_fsm` interpreter.
