# Transducers on streams

[`transducer.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/transducer.rs) provides [`Transducer`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/transducer.rs), a composable reducing-function transform. It is kernel-independent and is the same type `id_effect_optics::Transducer` re-exports.

```rust
use id_effect::{Exit, Stream, transducer_filter, transducer_map};

let xf = transducer_map(|n: i32| n + 1).compose(transducer_filter(|n: &i32| n % 2 == 0));
let out = Stream::<i32>::from_iterable(1..=5).via_transducer(xf).run_collect().block_on();
assert_eq!(out, Exit::succeed(vec![2, 4, 6]));
```

- **`via_transducer`** — alias of **`transduce_items`**: a map/filter pipeline per element, preserving order. The transducer keeps the element type (`Transducer<A, Vec<A>>`).
- **`outer.compose(inner)`** — wraps `inner`'s reducer in `outer`'s, so elements pass through `outer` first (above: `+ 1`, then the even filter).
- Built-ins: **`transducer_map`**, **`transducer_filter`**, **`transducer_take`** (the crate-root names of `streaming::transducer::{map, filter, take}`).

For list-only transduction without streams, call `Transducer::transduce(iter, reducer, init)` directly.
