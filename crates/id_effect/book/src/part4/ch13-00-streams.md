# Streams — Backpressure and Chunked Processing

An `Effect` produces one value. A `Stream` produces many values over time. When you need to process a potentially infinite or very large sequence — database result sets, event logs, file lines, sensor readings — `Stream` is the right abstraction.

`Stream<A, E = Never, R = HNil>` has the same type parameters as `Effect`. It is **pull-based**:
nothing runs until a consumer (`run_collect`, `run_fold`, a `Sink`, …) turns the stream into an
`Effect`, and each pull asks for at most one chunk. Operators run on the consumer's fiber under
the consumer's environment `R`, and a running stream is interrupted like any other effect.

This chapter covers when to use `Stream` vs `Effect`, how streams move data in `Chunk`s, how to
bridge a producer with a backpressure policy, how to consume streams with `Sink`, and how the
Compute Fabric parallelises stream and bulk work. Runnable examples: `060_stream_range` …
`075_backpressure_decision` and `151_cookbook_streaming_etl` in `crates/id_effect/examples/`.
