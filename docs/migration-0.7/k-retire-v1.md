# Retiring the v1 kernel, part 1: tracing buffer, moved shared types, `id-effect-diagnose` removed

## In-process tracing buffer on the `next` kernel

The 0.6 tracing buffer (`install_tracing_layer`, `snapshot_tracing`, `EffectEvent`, …) now lives
in `id_effect::next` (`next::observability::tracing`) and is fed by a span hook instead of a v1
`with_span` wrapper:

```rust
// 0.6
use id_effect::{TracingConfig, install_tracing_layer, run_blocking, snapshot_tracing, with_span};
run_blocking(install_tracing_layer(TracingConfig::enabled()), ()).unwrap();
run_blocking(with_span(program, "request"), env);
let snap = snapshot_tracing();

// 0.7
use id_effect::next::{TracingConfig, install_tracing_layer, snapshot_tracing, tracing_hook};
install_tracing_layer(TracingConfig::enabled()); // a plain function now
let exit = program.with_span("request").with_span_hook(tracing_hook()).block_on();
let snap = snapshot_tracing();
```

- `install_tracing_layer` is a synchronous function (it only flips the switch and clears the
  buffer).
- Spans are recorded only under the `TracingHook` span hook (`Effect::with_span_hook` /
  `LocalEffect::with_span_hook`, inherited by forked children). A span's attributes
  (`with_span_attrs`) seed its `SpanRecord` annotations. A defect or an interrupt ends a span
  with `EffectEvent::Failure`, like a typed failure.
- `annotate_current_span(key, value)` is an `Effect<(), E, R>` for any `E`/`R` (no turbofish, no
  sealed success/error traits). `emit_effect_event` / `emit_fiber_event` / `emit_compute_event`
  are effect steps; `record_*` are their synchronous forms.
- The 0.6 API keeps working until the old kernel is deleted: it records into the same buffer.
- `id_effect_cli::RunMainConfig::with_tracing()` uses the new buffer: it installs it and runs the
  program under `tracing_hook()`, so the program's `with_span`s show up in `snapshot_tracing()`.

## Kernel-independent types moved

| Type | 0.6 home | 0.7 home | old path |
|---|---|---|---|
| `FiberId` | `concurrency::fiber_id` | `failure::fiber_id` (root `id_effect::FiberId`) | still re-exported |
| `Never` | `runtime::execute` | `failure::never` (root `id_effect::Never`) | still re-exported |
| `Chunk` | `streaming::chunk` | `next::streaming::chunk` | still re-exported |
| `Transducer`, `Reducer`, `map`/`filter`/`take` | `streaming::transducer` | `next::streaming::transducer` | still re-exported |

Import them from the crate root (`id_effect::{FiberId, Never}`) or from `id_effect::next`; the
`runtime`, `concurrency` and `streaming` paths go away with the old kernel.

## `id-effect-diagnose` removed

The `id-effect-diagnose` binary of `id_effect_cli` printed diagnostics for the 0.6 runtime
capability graph (`CapabilityGraph`, `ProviderSpec`, `plan_topological`). In 0.7 a missing or
cyclic capability is a compile error (typed `R` and `Layer`s), so the tool has nothing to check
and is gone, together with `id_effect_cli`'s `serde`, `serde_json` and `toml` dependencies.

## `examples/cli-minimal`

The template is a native `next` program now: `app_effect` returns an
`id_effect::next::Effect<(), ConfigError, ConfigEnv>` and its `ConfigEnv`, and `main` calls
`run_main(eff, env, RunMainConfig::with_tracing())` with no bridge. `load_token_secret` returns
`Result<Secret<String>, Cause<ConfigError>>`. The cookbook example (`153_cookbook_cli_edge`)
runs over an explicit capability list (`HCons<ConfigProviderService, HCons<ConsoleService,
HNil>>`, `ConsoleService` a newtype over `Arc<dyn Console>`) instead of a v1 `Env`.
