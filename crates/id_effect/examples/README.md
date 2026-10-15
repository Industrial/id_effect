# Effect.rs numbered examples (curriculum)

Examples are named `NNN_slug.rs` (three-digit order + short topic). Run from the repo root, for example:

- `cargo run -p id_effect --example 001_effect_value`
- `cargo run -p id_effect_tokio --example 109_tokio_end_to_end`

Every example runs on the 0.7 kernel (`Effect<A, E = Never, R = HNil>`, run at the edge with
`Effect::block_on`, or awaited with `Runtime::run`). Numbers that were dropped with the 0.6 kernel
(`ThreadSleepRuntime`, `CancellationToken`, `TestClock`, `run_test`, `#[named]` provider variants,
the snapshot corpus, …) are listed in `docs/migration-0.7/k-retire-v1.md`.

## Spine (`id_effect` crate, 001–105)

| # | File | Topic |
|---|------|--------|
| 001 | `001_effect_value.rs` | `Effect` as a lazy value, `succeed`, `block_on` |
| 002 | `002_fail_boundary.rs` | `fail`, `Exit::Failure` at the boundary |
| 003–005 | `003_map` … `005_pipe` | `map`, `flat_map`, `.pipe` / `pipe!` |
| 006–010 | `006_effect_macro_binds` … `010_one_effect_macro_per_fn` | `effect!` binds (`~expr`), `?` on a `Result` |
| 008 | `008_effect_macro_env.rs` | `~Service` capability read, `provide` |
| 011–018 | `011_map_error` … `018_exit_type` | `map_err`, `catch`, `From` widening, `err!` / `Or`, `Cause`, `Exit` |
| 037–039 | `037_from_async_basic` … `039_run_async` | `from_async`, `Runtime::run` |
| 040 | `040_capability_app.rs` | typed `R`, `derive(ProviderSpecDerive)` → `layer()`, `provide_layer` |
| 042 | `042_effectful_config_provider.rs` | an effectful `Layer` |
| 048–051 | `048_run_fork_join` … `051_fiber_ref` | `fork` / `join`, `all`, `FiberRef` |
| 052–055 | `052_schedule_repeat_n` … `055_schedule_exhausted` | `Effect::repeat` / `Effect::retry` with `Schedule` |
| 060–075 | `060_stream_range` … `075_backpressure_decision`, `071_stream_map_serial` | `Stream`, channels, broadcast, backpressure |
| 077–082 | moved to `crates/id_effect_fp/examples/` | STM (`atomically`, `TRef`, `TMap`, …) — `cargo run -p id_effect_fp --example 077_stm_tref` |
| 083–088 | `083_schema_primitive` … `088_brand_equal_hash` | `Schema`, `EffectData`, `Brand` |
| 089–091 | `089_ensuring` … `091_scope_with` | `ensuring`, `acquire_use_release`, `Effect::scoped` finalizers (`090`–`092_algebra_*` moved to `id_effect_fp`) |
| 093–096 | `093_tracing_install` … `096_tracing_snapshot` | in-process tracing buffer, `with_span` + `tracing_hook` |
| 100–105 | `100_channel_queue` … `105_match_matcher` | `Channel`, `PubSub`, `Semaphore`, `KeyedPool`, `Cache`, `Matcher` |

## Compute Fabric (120–122, feature `compute`)

| # | File | Topic |
|---|------|--------|
| 120 | `120_compute_fabric_memory_cap.rs` | admission from telemetry vs policy; `Fabric::from_compute` + `Concurrency::Inherit` |
| 121 | `121_compute_fabric_cpu_spread.rs` | spread mode, `CpuSpreadBucket`, compute events in the tracing buffer |
| 122 | `122_compute_fabric_effect_parallel.rs` | `Stream::map_effect_par` under a fabric-backed `Fabric` |

## Tokio adapter (`id_effect_tokio`, 106–109)

| # | File | Topic |
|---|------|--------|
| 106 | `106_tokio_runtime.rs` | `TokioRuntime`, `sleep`, `yield_fiber` |
| 107 | `107_tokio_fork_contract.rs` | fork contract |
| 108 | `108_tokio_clock.rs` | time with an external Tokio runtime |
| 109 | `109_tokio_end_to_end.rs` | capabilities, `from_async`, full pipeline |

## Cookbook (151–154, book "Cookbook" part)

| # | Package / file | Topic |
|---|----------------|-------|
| 151 | `151_cookbook_streaming_etl.rs` | pull-based ETL, `tumbling`, bounded memory on an infinite source (has `#[test]`s: `cargo test -p id_effect --example 151_cookbook_streaming_etl`) |
| 152 | `id_effect_platform` · `152_cookbook_bounded_command_pool.rs` | N commands, at most K in flight (`Semaphore` + `platform::command`) |
| 153 | `cli_minimal` · `153_cookbook_cli_edge.rs` | `run_main`, config from env, capabilities at the edge |
| 154 | `id_effect_process` · `154_cookbook_supervised_daemon.rs` | supervisor restart + graceful shutdown |

Curriculum work is tracked with **tool-tasks** (see `.cursor/rules/tool-tasks.mdc`).
