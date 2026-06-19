---
name: elixir-concurrency
description: >-
  Implements concurrent and parallel data processing in Elixir using Task,
  Flow, GenStage, and Broadway with correct back-pressure. Use when building
  pipelines, batch processing, rate-limited workers, or when the user mentions
  concurrency, Flow, GenStage, Broadway, or back-pressure.
---

# Elixir Concurrency

From *Concurrent Data Processing in Elixir* and *Elixir in Action*.

## Selection tree

```
Need concurrency?
├─ Independent one-off jobs → Task.async / Task.async_stream
├─ Bounded parallel map → Task.async_stream(max_concurrency: n, timeout: ...)
├─ Producer/consumer, multiple stages → GenStage or Flow
└─ High-throughput ingestion (Kafka, SQS, etc.) → Broadway
```

## Task

```elixir
Task.async(fn -> expensive(id) end)
|> Task.await(timeout)

# Parallel collection
ids
|> Task.async_stream(&process/1, max_concurrency: 10, timeout: :infinity)
|> Enum.to_list()
```

- Always supervise under `Task.Supervisor` when tasks outlive the caller.
- Set `max_concurrency` explicitly — default can overwhelm DB or APIs.

## Flow

Built on GenStage; use for **map-reduce style** over enumerables with back-pressure:

```elixir
File.stream!(path)
|> Flow.from_enumerable()
|> Flow.map(&parse/1)
|> Flow.partition(key: &key/1)
|> Flow.reduce(fn -> acc end, &fold/2)
|> Flow.run()
```

Requires `max_demand` / stages tuning for external bottlenecks.

## GenStage

When you need **custom producers/consumers** or multiple heterogeneous stages:

- Producer controls demand (`handle_demand`).
- Consumers subscribe with `:max_demand` and `:min_demand`.
- Use when Flow's API is insufficient (dynamic producers, complex subscriptions).

## Broadway

For **message-driven** pipelines (Kafka, RabbitMQ, SQS, Redis):

- Broadway handles concurrency, batching, acknowledgements, failure.
- Processors + batchers; configure `concurrency`, `max_demand`, `batch_size`.
- Failed messages: `Broadway.Message.failed/2` and retry/DLQ strategy.

## Back-pressure rules

1. Producer must not outrun slow consumers without bound.
2. Match concurrency to **downstream capacity** (DB pool size, API rate limits).
3. Prefer streaming (`Stream`, `Flow.from_enumerable`) over loading full lists.
4. Timeouts on external calls inside workers.

## Process vs data parallelism

| Model | Tool |
|-------|------|
| Same code, many items | `Task.async_stream`, Flow |
| Long-lived pipeline stages | GenStage, Broadway |
| Serialized access to resource | GenServer or `:gen_statem` gate |

## Testing

- Test stage logic as pure functions where possible.
- Integration tests with small batches and `start_supervised` Broadway pipelines.
- Avoid `:timer.sleep` — use message sync or Broadway test helpers.

## Additional resources

- Pipeline patterns: [reference/pipelines.md](reference/pipelines.md)

## Anti-patterns

| Avoid | Prefer |
|-------|--------|
| `Task.async` in loop without await | `Task.async_stream` |
| Unbounded `spawn` per message | Broadway / supervised pool |
| Loading entire file into memory | `File.stream!` + Flow |
| Ignoring DB pool size in concurrency | `pool_size` ≥ worker count |
