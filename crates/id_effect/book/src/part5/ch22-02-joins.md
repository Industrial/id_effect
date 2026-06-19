# Stream joins

[`join`](../../src/streaming/join.rs) combines multiple streams.

## Fair merge

[`Stream::merge`](../../src/streaming/join.rs) alternates elements from two sources:

```rust
use id_effect::Stream;

let merged = Stream::from_iterable([1, 2]).merge(Stream::from_iterable([10, 20]));
// [1, 10, 2, 20]
```

## Combine latest

[`combine_latest`](../../src/streaming/join.rs) keeps the latest value from each side and emits whenever either updates (after both have emitted at least once):

```rust
use id_effect::{Stream, combine_latest};

let pairs = combine_latest(
    Stream::from_iterable([1, 2]),
    Stream::from_iterable(['a', 'b']),
);
// [(2, 'a'), (2, 'b')]
```

## Keyed join

[`keyed_join`](../../src/streaming/join.rs) performs an inner join on the latest value per key:

```rust
use id_effect::{Stream, keyed_join};

let joined = keyed_join(
    Stream::from_iterable([("user", 1), ("other", 2)]),
    Stream::from_iterable([("user", 'x')]),
);
// [("user", 1, 'x')]
```
