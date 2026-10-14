# `id_effect_cli`

Thin helpers for building **CLIs whose bodies are [`Effect`](https://docs.rs/id_effect/latest/id_effect/kernel/struct.Effect.html) programs**:

- Map [`Exit`](https://docs.rs/id_effect/latest/id_effect/enum.Exit.html) / [`Cause`](https://docs.rs/id_effect/latest/id_effect/enum.Cause.html) / `Result` to [`std::process::ExitCode`].
- [`run_main`](https://docs.rs/id_effect_cli/latest/id_effect_cli/fn.run_main.html) — optional tracing install, then [`run_blocking`](https://docs.rs/id_effect/latest/id_effect/runtime/fn.run_blocking.html), then stderr logging and exit code mapping.
- Optional [`clap`](https://docs.rs/clap) (feature `clap`, on by default).

## Quick start

```rust
use std::process::ExitCode;

use id_effect::{Cause, Effect, Exit, FiberId, fail, succeed};
use id_effect_cli::{RunMainConfig, exit_code_for_exit, run_main};

fn greet(name: &str) -> Effect<(), String, ()> {
  if name.is_empty() {
    fail("name must not be empty".to_string())
  } else {
    let line = format!("hello, {name}");
    succeed(()).map(move |()| println!("{line}"))
  }
}

fn main() -> ExitCode {
  // Ok -> 0, Err(e) -> prints `error: {e:?}` to stderr and exits 1.
  assert_eq!(run_main(greet("ada"), (), RunMainConfig::minimal()), ExitCode::SUCCESS);
  assert_eq!(run_main(greet(""), (), RunMainConfig::minimal()), ExitCode::from(1));

  // With a full `Exit`, defects and interrupts keep their own codes.
  assert_eq!(exit_code_for_exit(Exit::<(), String>::Failure(Cause::die("bug"))), ExitCode::from(101));
  assert_eq!(
    exit_code_for_exit(Exit::<(), String>::Failure(Cause::interrupt(FiberId::new(1)))),
    ExitCode::from(130),
  );
  ExitCode::SUCCESS
}
```

| Outcome | Exit code |
|---------|-----------|
| success | `0` |
| typed failure (`Err`, `Cause::Fail`) | `1` |
| defect (`Cause::Die`) | `101` |
| interrupt (`Cause::Interrupt`) | `130` |
| `Cause::Both` / `Cause::Then` | strongest of the two |

## Template

[`examples/cli-minimal`](../../examples/cli-minimal/) is **the** starter template for a new id_effect CLI
(clap + `id_effect_config` secrets + `run_main`). CI (`scripts/check-version-drift.sh`) keeps its
`id_effect*` dependency versions in lockstep with the workspace release.

## Examples

```bash
cargo run -p id_effect_cli --example 010_exit_codes -- fail; echo "exit=$?"
```

See also the mdBook chapter **CLI entrypoints (`id_effect_cli`)** and crate docs on [docs.rs](https://docs.rs/id_effect_cli).
