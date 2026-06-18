# Effect.rs numbered examples (curriculum)

Examples are named `NNN_slug.rs` (three-digit order + short topic). Run from the repo root, for example:

- `cargo run -p id_effect --example 001_effect_value`
- `cargo run -p id_effect_tokio --example 109_tokio_end_to_end`

## Spine (`effect` crate, 001–105)

| # | File | Topic |
|---|------|--------|
| 001 | `001_effect_value.rs` | `Effect` as lazy value, `succeed`, `run_blocking` |
| 002 | `002_fail_boundary.rs` | `fail`, `Err` at boundary |
| 003–005 | `003_map` … `005_pipe` | `map`, `flat_map`, `pipe!` |
| 006–010 | `006_effect_macro_binds` … `010_one_effect_macro_per_fn` | `effect!` / `Result` bind |
| 011–018 | `011_map_error` … `018_exit_type` | errors, `Or`, `Cause`, `Exit` |
| 040 | `040_capability_app.rs` | capability DI: `#[capability]`, `ProviderSpec`, `run_with` |
| 041 | `041_scoped_config_provider.rs` | `Env::scoped` config override (replaces ambient) |
| 008 | `008_effect_macro_env.rs` | `effect!` + `require!` with capability [`Env`] |
| 034, 036 | archived — legacy layer examples in `archived/` |
| 037–041 | `037_from_async_basic` … `041_scoped_config_provider` | async effects, scoped config |
| 042 | `042_effectful_config_provider.rs` | effectful `provide_effect` provider |
| 043 | `043_named_variant_providers.rs` | `#[named]` primary/replica variants |
| 043–052 | `043_cancellation_token` … `052_schedule_repeat_n` | cancellation, fibers (`FiberRef`, `fiber_all`), `repeat_n` |
| 053–059 | `053_schedule_repeat` … `059_schedule_interrupt` | `Schedule`, clocks, interrupts |
| 060–076 | `060_stream_range` … `076_stream_duplex_queue`, `071_stream_map_serial` | `Stream`, `from_iterable`, duplex queue, backpressure |
| 077–082 | `077_stm_tref` … `082_stm_tsemaphore` | STM (`atomically`, `TRef`, `TMap`, …) |
| 083–088 | `083_schema_primitive` … `088_brand_equal_hash` | `Schema`, `EffectData`, `Brand` |
| 089–092 | `089_ensuring` … `092_scoped` | `ensuring`, acquire/release, `scope_with`, `scoped` |
| 093–099 | `093_tracing_install` … `099_snapshot_corpus` | tracing + test harness |
| 100–105 | `100_channel_queue` … `105_match_matcher` | `Channel`, `PubSub`, pool, cache, `Matcher` |

## Tokio adapter (`id_effect_tokio`, 106–109)

| # | File | Topic |
|---|------|--------|
| 106 | `106_tokio_runtime.rs` | `TokioRuntime`, `sleep`, `yield_now` |
| 107 | `107_tokio_fork_contract.rs` | `run_fork` contract |
| 108 | `108_tokio_clock.rs` | `now` / time with external Tokio runtime |
| 109 | `109_tokio_end_to_end.rs` | capability DI, streams, `catch`, full pipeline |

Curriculum work is tracked with **tool-tasks** (see `.cursor/rules/tool-tasks.mdc`).
