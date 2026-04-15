# `streaming` — Stratum 11: chunked effectful streams

**Lazy, chunked sequences** with backpressure: [`Chunk`](chunk.rs), [`Sink`](sink.rs), and [`Stream`](stream.rs) plus helpers (`stream_from_channel`, `send_chunk`, `BackpressurePolicy`, …).

## What lives here

| Module | Role |
|--------|------|
| `chunk` | `Chunk` — batching items for efficient processing. |
| `sink` | `Sink` — consumers of chunks (often tied to channels or IO). |
| `stream` | `Stream`, `StreamSender`, policies, fan-out/broadcast types — core streaming API. |

## What it is used for

- **Bridging** [`coordination::Channel`](../coordination/README.md) (and similar) into composable pipelines.
- **Applying** backpressure when producers outrun consumers (`BackpressurePolicy`, `backpressure_decision`).
- **Broadcasting** to multiple subscribers where the crate provides fan-out helpers.

## Best practices

1. **Choose policy explicitly** — default backpressure may not match your SLO; document drops vs block.
2. **End streams cleanly** with `end_stream` so downstream sinks observe completion.
3. **Avoid unbounded buffers** in hot paths — pair with semaphores or bounded queues.
4. **Test** slow-consumer scenarios; streaming bugs often appear only under load.

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 11.
- [`coordination`](../coordination/README.md) — channels feeding streams.
- [`observability`](../observability/README.md) — metrics on chunk rates and drops.
