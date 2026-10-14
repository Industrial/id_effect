# Transducers on streams

[`transducer`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/transducer.rs) provides [`Transducer`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/transducer.rs) with the same shape as [`id_effect_optics::Transducer`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_optics/src/transducer.rs) (kept local to avoid a crate dependency cycle).

```rust
use id_effect::{Stream, Transducer, transducer_filter, transducer_map};

let xf = transducer_map(|n: i32| n + 1).compose(transducer_filter(|n: &i32| n % 2 == 0));
let out = Stream::from_iterable(1..=5).via_transducer(xf);
// [2, 4, 6]
```

- **`via_transducer`** — alias of **`transduce_items`**: map/filter pipeline per element, preserving order.
- **`Transducer::compose`** — inner transducer runs first, then outer (same as optics).
- Built-ins: **`transducer_map`**, **`transducer_filter`**.

For list-only transduction without streams, use `id_effect_optics` directly or `Transducer::transduce` on iterators.
