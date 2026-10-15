# Stream joins

[`join`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/join.rs) combines multiple streams.

## Fair merge

[`Stream::merge`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/join.rs) alternates elements from two sources:

```rust
use id_effect::{Exit, Stream};

let merged: Stream<i32> = Stream::from_iterable([1, 2]).merge(Stream::from_iterable([10, 20]));
assert_eq!(merged.run_collect().block_on(), Exit::succeed(vec![1, 10, 2, 20]));
```

## Combine latest

[`combine_latest`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/join.rs) keeps the latest value from each side and emits whenever either updates (after both have emitted at least once):

```rust
use id_effect::{Exit, Stream, combine_latest};

let pairs: Stream<(i32, char)> = combine_latest(
    Stream::from_iterable([1, 2]),
    Stream::from_iterable(['a', 'b']),
);
assert_eq!(pairs.run_collect().block_on(), Exit::succeed(vec![(2, 'a'), (2, 'b')]));
```

## Keyed join

[`keyed_join`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/join.rs) performs an inner join on the latest value per key:

```rust
use id_effect::{Exit, Stream, keyed_join};

let joined: Stream<(&str, i32, char)> = keyed_join(
    Stream::from_iterable([("user", 1), ("other", 2)]),
    Stream::from_iterable([("user", 'x')]),
);
assert_eq!(joined.run_collect().block_on(), Exit::succeed(vec![("user", 1, 'x')]));
```
