# Cookbook

Short recipes for jobs that come up in real services, daemons and CLIs. Each recipe is a runnable
example in the repository. It finishes in well under a second, uses no real sleeps, and asserts its
own result, so the code shown here compiles and runs as written.

| Recipe | Crates | Example |
|--------|--------|---------|
| [Retry with `TestClock`](./ch40-01-retry-test-clock.md) | `id_effect`, `id_effect_resilience` | `150_cookbook_retry_test_clock`, `010_retry_with_breaker` |
| [Bounded subprocess pool](./ch40-02-bounded-subprocess-pool.md) | `id_effect`, `id_effect_platform` | `152_cookbook_bounded_command_pool` |
| [A `run_blocking` CLI](./ch40-03-cli-run-main.md) | `id_effect_cli`, `id_effect_config` | `153_cookbook_cli_edge` |
| [Supervised daemon skeleton](./ch40-04-supervised-daemon.md) | `id_effect_process` | `154_cookbook_supervised_daemon` |
| [Streaming ETL with bounded memory](./ch40-05-streaming-etl.md) | `id_effect` | `151_cookbook_streaming_etl` |

Run them all from the repository root:

```bash
cargo run -p id_effect --example 150_cookbook_retry_test_clock
cargo run -p id_effect_resilience --example 010_retry_with_breaker
cargo run -p id_effect_platform --example 152_cookbook_bounded_command_pool
cargo run -p cli_minimal --example 153_cookbook_cli_edge -- --name Ada
cargo run -p id_effect_process --example 154_cookbook_supervised_daemon
cargo run -p id_effect --example 151_cookbook_streaming_etl
```

All five follow one pattern:

1. Write the program as an `Effect` that names what it needs (`R`, or `Needs<…>` bounds).
2. Provide the real capabilities once, at the edge (`main`).
3. In tests, provide fakes instead: a `TestClock`, a `MapConfigProvider`, a recording console, or
   an instrumented source.
