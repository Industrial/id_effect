# Chunks — Efficient Batched Processing

A `Stream` doesn't emit elements one at a time at the memory level. It emits them in `Chunk`s — contiguous, fixed-capacity batches. Most of the time you don't interact with `Chunk` directly; the stream API works element-wise and handles chunking internally. But understanding chunks helps you tune performance.

## What Is a Chunk

```rust
use id_effect::Chunk;

// A Chunk is a fixed-capacity contiguous sequence
let chunk: Chunk<i32> = Chunk::from_vec(vec![1, 2, 3, 4, 5]);

// Access elements
let first: Option<&i32> = chunk.first();
let len: usize = chunk.len();

// Iterate
for item in &chunk {
    println!("{item}");
}
```

A `Chunk<A>` is essentially a smart `Arc<[A]>` slice: cheap to clone (reference-counted), cache-friendly (contiguous layout), and zero-copy when slicing.

## Why Chunks Exist

Processing elements one at a time through a chain of `.map` and `.filter` calls has overhead: each step is a separate allocation or indirection. Chunks amortize that cost:

```
Single-element model:
  elem1 → map → filter → emit → elem2 → map → filter → emit → ...
  (N function calls for N elements through each operator)

Chunk model:
  chunk[1..64] → map_chunk → filter_chunk → emit_chunk → ...
  (N/64 overhead calls; SIMD-friendly layout)
```

The default chunk size is 64 elements. You can change it when constructing a stream:

```rust
let stream = Stream::from_iter(0..1_000_000)
    .with_chunk_size(256);
```

Larger chunks improve throughput for CPU-bound map/filter operations. Smaller chunks reduce latency when downstream consumers need to act quickly.

## Working with Chunks Directly

Most operators are element-wise, but a few operate at the chunk level for efficiency:

```rust
// map_chunks: apply a function to entire chunks at once
stream.map_chunks(|chunk| {
    chunk.map(|x| x * 2)  // vectorizable
})

// flat_map_chunks: emit a new chunk per input chunk
stream.flat_map_chunks(|chunk| {
    Chunk::from_iter(chunk.iter().flat_map(expand))
})
```

Use `map_chunks` when your transformation is pure and benefits from batching (e.g., numeric processing, serialisation).

## Chunk in Sinks and Collectors

When a `Sink` receives data, it receives `Chunk`s. Custom sinks that write to a file or network socket often want to write whole chunks at once:

```rust
impl Sink<Bytes> for FileSink {
    fn on_chunk(&mut self, chunk: Chunk<Bytes>) -> Effect<(), IoError, ()> {
        // write all bytes in one system call
        effect! {
            for bytes in &chunk {
                ~ self.write(bytes);
            }
            ()
        }
    }
}
```

## Building Chunks

```rust
// From an iterator
let chunk = Chunk::from_iter([1, 2, 3]);

// From a Vec (no copy if Vec capacity matches)
let chunk = Chunk::from_vec(v);

// Empty chunk
let empty: Chunk<i32> = Chunk::empty();

// Single element
let one = Chunk::single(42);

// Concatenate two chunks (zero-copy if they're adjacent)
let combined = Chunk::concat(chunk_a, chunk_b);
```

## Summary

You rarely construct `Chunk` by hand in application code. The stream runtime handles chunking for you. Understand chunks when:

- You're writing a custom `Sink` and want efficient writes
- You're tuning throughput with `.with_chunk_size(n)`
- You're implementing a library operator with `map_chunks`
