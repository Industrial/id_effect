# A CLI with `run_main`

**Problem.** You are writing a command-line tool. You want typed errors, configuration from the
environment, testable logic, and correct exit codes, without an async `main` or a tangle of
`std::process::exit` calls.

**Recipe.** Keep `main` a thin edge:

1. parse argv with `clap`,
2. build one environment value holding the real capabilities (an `EnvConfigProvider`, a stdout
   console, …),
3. hand the program `Effect` (or a `!Send` `LocalEffect`) and that environment to
   `id_effect_cli::run_main`. It provides the environment, runs the effect at the blocking edge,
   prints a failure to stderr, and returns the `ExitCode` for its `Exit`.

The program names its capabilities in `R`, so it never knows where its configuration or output
goes:

```rust,no_run
# use std::process::ExitCode;
# #[derive(Debug)] pub enum AppError { Config(id_effect_config::ConfigError), TooManyGreetings(i64) }
# impl From<id_effect_config::ConfigError> for AppError { fn from(e: id_effect_config::ConfigError) -> Self { AppError::Config(e) } }
# impl std::fmt::Display for AppError {
#   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{self:?}") }
# }
# /// Stand-in for a `#[derive(clap::Parser)]` struct (clap is not a dependency of the book).
# struct Cli { name: String }
# impl Cli { fn parse() -> Self { Cli { name: "Ada".into() } } }
# struct Stdout;
# impl Console for Stdout { fn line(&self, text: String) { println!("{text}") } }
use std::sync::Arc;
use id_effect::{Effect, HCons, HNil, fail, need};
use id_effect_cli::{RunMainConfig, run_main};
use id_effect_config::{Config, ConfigProvider, ConfigProviderService, EnvConfigProvider};

pub trait Console: Send + Sync { fn line(&self, text: String); }

/// The console capability as it sits in the environment.
#[derive(Clone)]
pub struct ConsoleService(pub Arc<dyn Console>);

/// Everything the program needs: a config provider and a console.
pub type AppEnv = HCons<ConfigProviderService, HCons<ConsoleService, HNil>>;

fn greet(name: String) -> Effect<(), AppError, AppEnv> {
  Config::integer("GREET_TIMES")
    .with_default(1)
    .run::<i64, AppError, AppEnv, _>()
    .flat_map(move |times| {
      if !(1..=3).contains(&times) {
        return fail(AppError::TooManyGreetings(times));
      }
      need::<ConsoleService>().map(move |console| {
        for _ in 0..times { console.0.line(format!("hello, {name}")); }
      })
    })
}

/// The edge: the one place that picks real or fake capabilities.
fn edge_env(config: impl ConfigProvider + 'static, console: Arc<dyn Console>) -> AppEnv {
  HCons::new(ConfigProviderService(Arc::new(config)), HCons::new(ConsoleService(console), HNil))
}

fn main() -> ExitCode {
  let cli = Cli::parse();
  let env = edge_env(EnvConfigProvider::from_env(), Arc::new(Stdout));
  // `run_main` prints `Err` with `Debug`; map to `String` for a human-readable message.
  run_main(greet(cli.name).map_err(|e| e.to_string()), env, RunMainConfig::minimal())
}
```

`RunMainConfig::with_tracing()` also installs the in-process tracing buffer and runs the program
under `tracing_hook()`, so its `with_span`s show up in `snapshot_tracing()`.

Tests use the same `edge_env` with a `MapConfigProvider` and a recording console, then check the
exit code mapping directly:

```rust
# use std::process::ExitCode;
# use std::sync::{Arc, Mutex};
# use id_effect::{Effect, Exit, HCons, HNil, fail, need};
# use id_effect_cli::exit_code_for_result;
# use id_effect_config::{Config, ConfigProvider, ConfigProviderService, MapConfigProvider};
# #[derive(Debug)] pub enum AppError { Config(id_effect_config::ConfigError), TooManyGreetings(i64) }
# impl From<id_effect_config::ConfigError> for AppError { fn from(e: id_effect_config::ConfigError) -> Self { AppError::Config(e) } }
# pub trait Console: Send + Sync { fn line(&self, text: String); }
# #[derive(Clone)] pub struct ConsoleService(pub Arc<dyn Console>);
# pub type AppEnv = HCons<ConfigProviderService, HCons<ConsoleService, HNil>>;
# fn greet(name: String) -> Effect<(), AppError, AppEnv> {
#   Config::integer("GREET_TIMES")
#     .with_default(1)
#     .run::<i64, AppError, AppEnv, _>()
#     .flat_map(move |times| {
#       if !(1..=3).contains(&times) {
#         return fail(AppError::TooManyGreetings(times));
#       }
#       need::<ConsoleService>().map(move |console| {
#         for _ in 0..times { console.0.line(format!("hello, {name}")); }
#       })
#     })
# }
# fn edge_env(config: impl ConfigProvider + 'static, console: Arc<dyn Console>) -> AppEnv {
#   HCons::new(ConfigProviderService(Arc::new(config)), HCons::new(ConsoleService(console), HNil))
# }
# #[derive(Default)] struct Recorder(Mutex<Vec<String>>);
# impl Console for Recorder { fn line(&self, text: String) { self.0.lock().unwrap().push(text) } }
let recorder = Arc::new(Recorder::default());
let env = edge_env(MapConfigProvider::from_pairs([("GREET_TIMES", "9")]), recorder.clone());
let result = match greet("Ada".into()).provide_env(env).block_on() {
  Exit::Success(()) => Ok(()),
  Exit::Failure(cause) => Err(cause.fail_option().expect("a typed failure")),
};
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

`run_main` maps the whole `Exit`:

| outcome | stderr | exit code |
|---|---|---|
| success | – | `0` |
| typed failure `Fail(e)` | `error: {e:?}` | `1` |
| defect `Die` | `error: defect: …` | `101` |
| interrupt | `error: interrupted (…)` | `130` |

Composite causes keep the strongest code. Call it from `main` only: inside an async context it
runs nothing and exits `101`. When you drive the program yourself (a fiber join, a supervisor),
map its `Exit` with `exit_code_for_exit`, or a `Result` with `exit_code_for_result`. See [Exit codes for `main`](../part3/ch16-01-cli-exit-codes.md) and
`cargo run -p id_effect_cli --example 010_exit_codes -- die`.

See also: [CLI with clap](../part3/ch16-00-cli-with-clap.md),
[Config + `Secret` from flags](../part3/ch16-02-cli-config-secret.md).
