# `id_effect_cli`: `run_main` runs effects

`run_main` is the CLI program edge. In 0.7 it takes an effect: an
`id_effect::Effect` (`Send`) or an `id_effect::LocalEffect` (`!Send`), plus the
environment for its capability list `R`. Pass `HNil` when the effect needs nothing. It runs the
effect with `block_on` on the default runtime (`Runtime::global()`). This is the one
blocking call site, allowlisted for G-nb. It then maps the whole `Exit` to the exit code.

```rust
// 0.6
use id_effect::{Effect, fail, succeed};
let eff: Effect<(), String, ()> = succeed(());
run_main(eff, (), RunMainConfig::minimal())

// 0.7
use id_effect::{Effect, HNil, fail, succeed};
let eff: Effect<(), String> = succeed(());
run_main(eff, HNil, RunMainConfig::minimal())
```

A program over capabilities passes its typed environment (an `HCons` list) as `env`;
`examples/cli-minimal` runs an `Effect<(), ConfigError, ConfigEnv>` with its `ConfigEnv`. There
is no v1 → next bridge any more: a 0.6 program is ported to an effect (k-retire-v1).

## Behaviour changes

- `run_main` now maps the full `Exit`, not just `Result`. A defect (a `die` or a panic inside the
  effect) exits `101` and an interrupt exits `130`. In 0.6 a panic unwound out of `main`, and an
  interrupt was not observable. A typed failure still exits `1` and prints `error: {e:?}`.
  Defects print `error: defect: …` and interrupts print `error: interrupted (…)`.
- Called inside an async context (a tokio task), `run_main` runs nothing and exits `101` (the
  `block_on` refusal is a defect). Call it from `main`.
- The env bound is now `R: Send + Sync + 'static`, the capability list.
- `exit_code_for_exit`, `exit_code_for_cause`, `cause_max_exit_byte` and `exit_code_for_result`
  are unchanged. `Exit` and `Cause` are the shared root types `id_effect::{Exit, Cause}`.
- `RunMainConfig::with_tracing()` installs the in-process tracing buffer
  (`id_effect::install_tracing_layer`) and runs the program under `tracing_hook()`, so its
  `with_span`s show up in `snapshot_tracing()` (k-retire-v1 part 1; no bridge).
- The `id-effect-diagnose` binary is removed (k-retire-v1): it checked the 0.6 runtime provider
  graph, and a missing capability is a compile error in 0.7.
- The `ie_cli_probe` test binary gained `--mode die` (exit `101`).
