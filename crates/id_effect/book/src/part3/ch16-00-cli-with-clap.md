# CLI entrypoints with `clap` + `Effect`

Rust’s standard answer for parsing is **[`clap`](https://docs.rs/clap)**. For **id_effect**, treat the binary as a **thin shell**:

1. **Parse** argv into a struct (`#[derive(clap::Parser)]`).
2. **Assemble** the environment your program needs — its capability list `R` (`HNil`, a single capability, or an `HCons` list such as `id_effect_config::ConfigEnv`) — from flags and process environment.
3. **Run** your program `Effect<A, E, R>` at the edge with [`id_effect_cli::run_main`](https://docs.rs/id_effect_cli/latest/id_effect_cli/fn.run_main.html), which provides the environment and calls `block_on` on the default runtime.
4. **Exit** with the [`std::process::ExitCode`](https://doc.rust-lang.org/std/process/struct.ExitCode.html) it returns (see the next sections).

This mirrors Effect.ts **`@effect/cli`**: declarative argument models composed into a single program — here the “program” is an `Effect` value rather than a `Future` chain.

## Recommended `main` shape

Returning `ExitCode` keeps `main` testable and avoids calling [`std::process::exit`](https://doc.rust-lang.org/std/process/fn.exit.html) deep inside library code. This is `examples/cli-minimal/src/main.rs`:

```rust,no_run
use clap::Parser;
use id_effect_cli::{RunMainConfig, run_main};

#[derive(Parser, Debug)]
struct Cli {
    /// API token (loaded into config as `API_TOKEN`, then wrapped as a `Secret`).
    #[arg(long)]
    token: String,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let (eff, env) = cli_minimal::app_effect(cli.token); // Effect<(), ConfigError, ConfigEnv> + its ConfigEnv
    run_main(eff, env, RunMainConfig::with_tracing())
}
```

`run_main(effect, env, config)` accepts a `Send` `Effect` or a `!Send` `LocalEffect` over the capability list `R`; pass `HNil` when the effect needs nothing. A missing capability is a compile error, not a runtime failure. A complete version of this shape, with configuration from the environment and tests, is the cookbook example [`153_cookbook_cli_edge`](https://github.com/Industrial/id_effect/blob/main/examples/cli-minimal/examples/153_cookbook_cli_edge.rs) (`cargo run -p cli_minimal --example 153_cookbook_cli_edge -- --name Ada`).

Call `run_main` from `main` only: it parks the calling thread, and inside an async context (a tokio task) it runs nothing and exits `101`.

## Workspace helpers

The **`id_effect_cli`** crate provides:

- An optional **`clap`** dependency (feature `clap`, on by default).
- `run_main` — provides `env`, optionally installs the in-process tracing buffer (`RunMainConfig::with_tracing()` runs the program under `tracing_hook()`, so its `with_span`s show up in `snapshot_tracing()`), runs the effect, reports a failure on **stderr**, and maps the whole `Exit` to an `ExitCode`. `RunMainConfig::minimal()` skips tracing.
- `exit_code_for_exit` / `exit_code_for_cause` / `exit_code_for_result` when you already have an `Exit`, a `Cause` or a `Result` (from `block_on` in a custom driver, a test, or a supervisor).

```rust
use std::process::ExitCode;
use id_effect::{Effect, HNil, fail, succeed};
use id_effect_cli::{RunMainConfig, run_main};

fn greet(name: &str) -> Effect<(), String> {
    if name.is_empty() {
        fail("name must not be empty".to_string())
    } else {
        let line = format!("hello, {name}");
        succeed(()).map(move |()| println!("{line}"))
    }
}

assert_eq!(run_main(greet("ada"), HNil, RunMainConfig::minimal()), ExitCode::SUCCESS);
assert_eq!(run_main(greet(""), HNil, RunMainConfig::minimal()), ExitCode::from(1));
```

## Template

See the checked-in example **[`examples/cli-minimal`](https://github.com/Industrial/id_effect/tree/main/examples/cli-minimal)** (`--token …`, `Secret` via [`id_effect_config`](../part2/ch07-10-config.md)).

## Further reading

- [Exit codes for `main`](./ch16-01-cli-exit-codes.md) — `Exit` / `Cause` → `ExitCode` table
- [Config + `Secret` from flags](./ch16-02-cli-config-secret.md) — wiring `id_effect_config` at the edge
- [Error handling](./ch08-00-error-handling.md) — `Cause` vs plain `E`
- [Configuration (`id_effect_config`)](../part2/ch07-10-config.md)
