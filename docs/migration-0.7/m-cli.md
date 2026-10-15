# `id_effect_cli`: `run_main` runs `next` effects

`run_main` is the CLI program edge. In 0.7 it takes a `next` effect: an
`id_effect::next::Effect` (`Send`) or an `id_effect::next::LocalEffect` (`!Send`), plus the
environment for its capability list `R`. Pass `HNil` when the effect needs nothing. It runs the
effect with `block_on` on the default `next` runtime (`Runtime::global()`). This is the one
blocking call site, allowlisted for G-nb. It then maps the whole `Exit` to the exit code.

```rust
// 0.6
use id_effect::{Effect, fail, succeed};
let eff: Effect<(), String, ()> = succeed(());
run_main(eff, (), RunMainConfig::minimal())

// 0.7
use id_effect::next::{Effect, HNil, fail, succeed};
let eff: Effect<(), String> = succeed(());
run_main(eff, HNil, RunMainConfig::minimal())
```

A program that is still a v1 effect (for example one built on `id_effect_config` before its
switch-over) is bridged at the edge with `id_effect::next::compat::IntoNext`:

```rust
use id_effect::next::HNil;
use id_effect::next::compat::IntoNext;

run_main(v1_effect.into_next_with(env), HNil, cfg) // a v1 effect over env `R`
run_main(v1_effect.into_next(), HNil, cfg)         // a v1 effect over `()`
```

## Behaviour changes

- `run_main` now maps the full `Exit`, not just `Result`. A defect (a `die` or a panic inside the
  effect) exits `101` and an interrupt exits `130`. In 0.6 a panic unwound out of `main`, and an
  interrupt was not observable. A typed failure still exits `1` and prints `error: {e:?}`.
  Defects print `error: defect: …` and interrupts print `error: interrupted (…)`.
- Called inside an async context (a tokio task), `run_main` runs nothing and exits `101` (the
  `block_on` refusal is a defect). Call it from `main`.
- The env bound is now `R: Send + Sync + 'static`, the `next` capability list.
- `exit_code_for_exit`, `exit_code_for_cause`, `cause_max_exit_byte` and `exit_code_for_result`
  are unchanged. `Exit` and `Cause` are the shared root types `id_effect::{Exit, Cause}`.
- `RunMainConfig::with_tracing()` still installs the v1 tracing layer
  (`id_effect::install_tracing_layer`). It goes through the v1 → next bridge until the tracing
  module is ported.
- The `ie_cli_probe` test binary gained `--mode die` (exit `101`).
