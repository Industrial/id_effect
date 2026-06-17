# Concurrency Pipeline Patterns

## ETL batch (file → transform → DB)

```elixir
path
|> File.stream!([], 2048)
|> Stream.map(&Jason.decode!/1)
|> Stream.map(&Boundary.to_domain/1)
|> Stream.chunk_every(100)
|> Stream.each(&Repo.insert_all(Schema, &1))
|> Stream.run()
```

Add `Task.async_stream` only when CPU-bound per row justifies it.

## Rate-limited API fan-out

```elixir
urls
|> Task.async_stream(
  &HTTP.get/1,
  max_concurrency: 5,
  timeout: 30_000
)
|> Enum.map(fn {:ok, resp} -> handle(resp) end)
```

Wrap with retry + circuit breaker at boundary when APIs are flaky.

## GenStage skeleton

```elixir
defmodule MyProducer do
  use GenStage

  def start_link(_), do: GenStage.start_link(__MODULE__, :ok, name: __MODULE__)
  def init(:ok), do: {:producer, :queue.new(), dispatcher: GenStage.BroadcastDispatcher}

  def handle_demand(_demand, state) do
    # emit events from state
  end
end
```

Subscribe consumers with `{MyConsumer, max_demand: 10, min_demand: 5}`.

## Broadway skeleton

```elixir
defmodule MyBroadway do
  use Broadway

  def start_link(_opts) do
    Broadway.start_link(__MODULE__,
      name: __MODULE__,
      producer: [...],
      processors: [default: [concurrency: 10]],
      batchers: [default: [batch_size: 50, batch_timeout: 1000]]
    )
  end

  def handle_message(_, message, _) do
    # transform message
    message
  end

  def handle_batch(_, messages, _, _) do
    # bulk side effect
    messages
  end
end
```

## Choosing Flow vs Broadway

| Signal | Choice |
|--------|--------|
| Finite enumerable in memory or on disk | Flow |
| Continuous message queue | Broadway |
| Need ack/retry/DLQ semantics | Broadway |
| Simple parallel map | Task.async_stream |
