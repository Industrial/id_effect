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
| 019–025 | `019_tagged_cell` … `025_req_type` | context, `ctx!`, `req!`, `Get` / `get_mut` |
| 026–036 | `026_layer_fn` … `036_layer_graph_diagnostics` | `Layer`, `Stack`, services, graphs |
| 037–042 | `037_from_async_basic` … `042_yield_now` | async effects, `Runtime`, yield |
| 043–052 | `043_cancellation_token` … `052_schedule_repeat_n` | cancellation, fibers (`FiberRef`, `fiber_all`), `repeat_n` |
| 053–059 | `053_schedule_repeat` … `059_schedule_interrupt` | `Schedule`, clocks, interrupts |
| 060–076 | `060_stream_range` … `076_stream_duplex_queue` | `Stream`, `from_iterable`, duplex queue, backpressure |
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
| 109 | `109_tokio_end_to_end.rs` | `req!`, streams, `catch`, full pipeline |

Curriculum work is tracked with **tool-tasks** (see `.cursor/rules/tool-tasks.mdc`).
