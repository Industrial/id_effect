# Cookbook

Short recipes for jobs that come up in real services, daemons and CLIs. Each recipe is backed by a
runnable example in the repository that finishes in well under a second and asserts its own
result.

| Recipe | Crates | Example |
|--------|--------|---------|
| [Retry with virtual time](./ch40-01-retry-test-clock.md) | `id_effect`, `id_effect_resilience` | `010_retry_with_breaker`, `crates/id_effect/tests/schedule.rs` |
| [Bounded subprocess pool](./ch40-02-bounded-subprocess-pool.md) | `id_effect`, `id_effect_platform` | `152_cookbook_bounded_command_pool` |
| [A CLI with `run_main`](./ch40-03-cli-run-main.md) | `id_effect_cli`, `id_effect_config` | `153_cookbook_cli_edge` |
| [Supervised daemon skeleton](./ch40-04-supervised-daemon.md) | `id_effect_process` | `154_cookbook_supervised_daemon` |
| [Streaming ETL with bounded memory](./ch40-05-streaming-etl.md) | `id_effect` | `151_cookbook_streaming_etl` |

Run them from the repository root:

```bash
cargo run -p id_effect_resilience --example 010_retry_with_breaker
cargo run -p id_effect_platform --example 152_cookbook_bounded_command_pool
cargo run -p cli_minimal --example 153_cookbook_cli_edge -- --name Ada
cargo run -p id_effect_process --example 154_cookbook_supervised_daemon
cargo run -p id_effect --example 151_cookbook_streaming_etl
```

All five follow one pattern:

1. Write the program as an `Effect<A, E, R>` whose `R` lists the capabilities it needs
   (`caps!(A, B)` or an explicit `HCons<A, HCons<B, HNil>>`).
2. Provide the real capabilities once, at the edge (`main`): `.provide_env(..)`,
   `.provide_layer(..)` or `run_main(program, env, config)`.
3. In tests, provide fakes instead (a `MapConfigProvider`, a recording console, an instrumented
   source) and run time-based code on a `TestExecutor`, whose virtual clock moves only when the
   test advances it.
