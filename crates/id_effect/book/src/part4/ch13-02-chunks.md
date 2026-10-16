# Chunks — Batched Processing

A `Stream` doesn't move elements one at a time. Each pull yields a `Chunk` of at most the
requested number of elements (`DEFAULT_CHUNK_SIZE`, 64, when the consumer doesn't choose) together
with the rest of the stream. Most of the time you don't touch `Chunk` directly: the operators work
element-wise and handle chunking for you.

## What Is a Chunk

`Chunk<A>` is a small wrapper over a `Vec<A>` with chunk-level semantics. It is `Clone`, `Eq` and
`Hash` when `A` is, so chunks of hashable elements can be map keys.

```rust
use id_effect::Chunk;

let chunk: Chunk<i32> = Chunk::from_vec(vec![1, 2, 3]);
assert_eq!(chunk.len(), 3);
assert_eq!(chunk.iter().copied().sum::<i32>(), 6);

let doubled: Chunk<i32> = chunk.map(|x| x * 2);
assert_eq!(doubled.into_vec(), vec![2, 4, 6]);

let empty: Chunk<i32> = Chunk::empty();
let one = Chunk::singleton(42);
```

`sort_with(&order)` and `compare_by(&other, &order)` take a runtime `DynOrder` from
`id_effect::schema::order`.

## Why Chunks Exist

Running every element through a chain of operators one by one costs one dispatch per element per
operator. Chunks amortise that: each operator stage runs once per chunk, and the pure `map` /
`filter` can hand a large enough chunk to Rayon through the Compute Fabric (see
[Parallelism](./ch13-05-parallelism.md)).

## Pulling by Hand

A consumer pulls with `next_chunk` (or `next_chunk_with_size(n)`), an effect yielding
`Option<(Chunk<A>, Stream<A, E, R>)>`: `None` at the end, otherwise one chunk and the rest.

```rust
use id_effect::{Exit, Stream};

let s = Stream::<i32, ()>::from_iterable(1..=5);
let (first, rest) = s.next_chunk_with_size(2).block_on().into_result().unwrap().unwrap();
assert_eq!(first.into_vec(), vec![1, 2]);
assert_eq!(rest.run_collect().block_on(), Exit::succeed(vec![3, 4, 5]));
```

A size of `0` counts as `1`. A failed pull returns no rest, so a stream cannot be pulled again
after a failure.

## Chunks at the Edges

Producers that bridge into a stream send whole chunks (`send_chunk(&sender, Chunk::from_vec(..))`,
see [Backpressure](./ch13-03-backpressure.md)), and `Stream::from_chunk(chunk)` turns one chunk
into a stream. `grouped(n)` / `tumbling(n)` regroup elements into `Vec`s of your chosen size when
downstream work wants explicit batches (example `065_stream_grouped`).

## Summary

You rarely build a `Chunk` by hand in application code. Reach for chunks when you send batches
from a producer, pull a stream manually, or write a library operator.
