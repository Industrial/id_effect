# Stream joins

[`join`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/join.rs) combines multiple streams.

## Fair merge

[`Stream::merge`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/join.rs) alternates elements from two sources:

```rust
use id_effect::{Exit, Stream};

let merged: Stream<i32> = Stream::from_iterable([1, 2]).merge(Stream::from_iterable([10, 20]));
assert_eq!(merged.run_collect().block_on(), Exit::succeed(vec![1, 10, 2, 20]));
```

## Zip

[`Stream::zip`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/zip.rs) pairs elements **by position** and ends with the shorter side (the other side is dropped, unpulled, so it may be infinite). [`Stream::zip_with`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/zip.rs) combines each pair with a function instead of building a tuple:

```rust
use id_effect::{Exit, Stream};

let pairs = Stream::<u32>::from_iterable(1..).zip(Stream::from_iterable(['a', 'b', 'c']));
assert_eq!(
    pairs.run_collect().block_on(),
    Exit::succeed(vec![(1, 'a'), (2, 'b'), (3, 'c')])
);

let sums = Stream::<u32>::from_iterable([1, 2, 3])
    .zip_with(Stream::from_iterable([10, 20, 30]), |a, b| a + b);
assert_eq!(sums.run_collect().block_on(), Exit::succeed(vec![11, 22, 33]));
```

Each pull fetches at most one chunk from each side whose buffer is empty, so memory is one chunk per side. A failure of either side fails the zip.

## Combine latest

[`combine_latest`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/join.rs) (or the method form `left.combine_latest(right)`, Effect.ts `zipLatest`) keeps the latest value from each side and emits whenever either updates (after both have emitted at least once):

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
