# state_scan — FSM stepping

[`state_scan`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/transduce.rs) (also `Stream::state_scan`) folds a state over the elements and emits **only when the step returns `Some`**. The step is `FnMut(S, A) -> (S, Option<B>)`.

Contrast with [`Stream::scan`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/stream.rs), which emits on every element:

```rust
use id_effect::{Exit, Stream};

#[derive(Clone, Copy, PartialEq)]
enum Mode { Idle, Active }

let rising_edges = Stream::<u8>::from_iterable([0u8, 1, 1, 0, 1])
    .state_scan(Mode::Idle, |mode, x| {
        let next = if x > 0 { Mode::Active } else { Mode::Idle };
        let emit = matches!((mode, next), (Mode::Idle, Mode::Active));
        (next, emit.then_some(x))
    })
    .run_collect()
    .block_on();
assert_eq!(rising_edges, Exit::succeed(vec![1, 1])); // Idle → Active edges only
```

Use this for simple finite-state interpretations without pulling in the full `id_effect_fsm` interpreter.
