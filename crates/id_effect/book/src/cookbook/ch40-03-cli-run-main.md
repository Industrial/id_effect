# A `run_blocking` CLI

**Problem.** You are writing a command-line tool. You want typed errors, configuration from the
environment, testable logic, and correct exit codes, without an async `main` or a tangle of
`std::process::exit` calls.

**Recipe.** Keep `main` a thin edge:

1. parse argv with `clap`,
2. build one `Env` holding the real capabilities (an `EnvConfigProvider`, a stdout console, …),
3. hand the program `Effect` to `id_effect_cli::run_main`. It drives the effect with `run_blocking`,
   prints an `Err` to stderr, and returns an `ExitCode` (`Ok` → 0, `Err` → 1).

The program is generic over `Needs<…>`, so it never knows where its configuration or output goes:

```rust
use id_effect::{Cap, Effect, Env, Needs};
use id_effect_config::{Config, ConfigProvider, ConfigProviderService, config_env};

pub trait Console: Send + Sync { fn line(&self, text: String); }
pub type ConsoleService = Arc<dyn Console>;

fn greet<R>(name: String) -> Effect<(), AppError, R>
where
  R: Needs<ConfigProviderService> + Needs<ConsoleService> + 'static,
{
  Config::integer("GREET_TIMES")
    .with_default(1)
    .run::<i64, AppError, R>()
    .flat_map(move |times| {
      Effect::new(move |r: &mut R| {
        if !(1..=3).contains(&times) {
          return Err(AppError::TooManyGreetings(times));
        }
        let console = Needs::<ConsoleService>::need(r).clone();
        for _ in 0..times { console.line(format!("hello, {name}")); }
        Ok(())
      })
    })
}

/// The edge: the one place that picks real or fake capabilities.
fn edge_env(config: impl ConfigProvider + 'static, console: ConsoleService) -> Env {
  let mut env = config_env(config);
  env.insert::<Cap<ConsoleService>>(console);
  env
}

fn main() -> ExitCode {
  let cli = Cli::parse();
  let env = edge_env(EnvConfigProvider::from_env(), Arc::new(Stdout));
  // `run_main` prints `Err` with `Debug`; map to `String` for a human-readable message.
  run_main(greet(cli.name).map_error(|e| e.to_string()), env, RunMainConfig::minimal())
}
```

Tests use the same `edge_env` with a `MapConfigProvider` and a recording console, then check the
exit code mapping directly:

```rust
let recorder = Arc::new(Recorder::default());
let env = edge_env(MapConfigProvider::from_pairs([("GREET_TIMES", "9")]), recorder.clone());
let result = run_blocking(greet("Ada".into()), env);
assert!(matches!(result, Err(AppError::TooManyGreetings(9))));
assert_eq!(exit_code_for_result(result), ExitCode::from(1));
```

Full example (in the `examples/cli-minimal` template package):
[`examples/cli-minimal/examples/153_cookbook_cli_edge.rs`](https://github.com/Industrial/id_effect/blob/main/examples/cli-minimal/examples/153_cookbook_cli_edge.rs)

```bash
cargo run -p cli_minimal --example 153_cookbook_cli_edge -- --name Ada
GREET_TIMES=9 cargo run -p cli_minimal --example 153_cookbook_cli_edge   # exits 1
cargo test -p cli_minimal --example 153_cookbook_cli_edge
```

## Defects and interrupts

`run_main` sees a `Result`, so every failure exits `1`. When you drive the program yourself and get
a full `Exit` (from `run_test`, a fiber join, or a supervisor), use `exit_code_for_exit`. It
maps typed failures to `1`, defects (`Cause::Die`) to `101` and interrupts to `130`. Composite
causes keep the strongest code. See [Exit codes for `main`](../part3/ch16-01-cli-exit-codes.md) and
`cargo run -p id_effect_cli --example 010_exit_codes -- die`.

See also: [CLI with clap](../part3/ch16-00-cli-with-clap.md),
[Config + `Secret` from flags](../part3/ch16-02-cli-config-secret.md).
