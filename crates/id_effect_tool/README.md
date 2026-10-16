# `id_effect_tool`

The `id-effect` command. `id-effect new` generates a ready-to-run project on [`id_effect_kit`](../id_effect_kit) from one of four templates embedded in the binary, so it works the same after `cargo install`.

```sh
cargo install id_effect_tool          # installs the `id-effect` binary
id-effect new orders-api --template service
cd orders-api && cargo test && cargo run
```

The package is `id_effect_tool` because crates.io treats `id-effect` and `id_effect` as the same name.

## `id-effect new`

```text
id-effect new <NAME> --template service|worker|cli|cluster
              [--dest DIR] [--db none|pg] [--otel|--no-otel] [--local-workspace PATH]
```

| Template | What you get |
|---|---|
| `service` | An Axum app with `SecurityLayer::recommended` (CSRF, CSP, security headers, cookie key), `/health` and `/ready` (`HealthRegistry`), `/metrics` with `http_metrics_layer`, served until SIGINT/SIGTERM. `--db pg` adds a PostgreSQL client, a first migration and a `db` readiness check; `--otel` exports traces, metrics and logs over OTLP. The defaults (`--db none --no-otel`) boot with no other services running. Includes a Kamal `config/deploy.yml`. |
| `worker` | Named cron jobs in a `CronRegistry` (overlap policy, missed-run catch-up, run history) under `run_main`. |
| `cli` | clap, `run_main` and a `completions <shell>` subcommand (the `examples/cli-minimal` shape). |
| `cluster` | An `id_effect_node` mesh node hosting a process behavior, called remotely in the tests. |

Every project has a `Cargo.toml` depending on `id_effect_kit` (and `id_effect`: the derive and `effect!` macros expand to `::id_effect::` paths), `src/`, `tests/`, a multi-stage `Dockerfile`, `compose.yaml`, `.github/workflows/ci.yml` (fmt, clippy, test, docker build), `.env.example`, `.gitignore` and a README.

- `--dest DIR` — where to write (default `./<NAME>`). A non-empty directory is refused; nothing is written then.
- `--local-workspace PATH` — build against an id_effect checkout: appends a `[patch.crates-io]` section pointing each id_effect crate the project uses at `PATH/crates/<crate>`. `scripts/template-smoke.sh` uses it to test every template in CI.

## Library

The binary is a thin shell over this crate:

```rust
use id_effect_tool::{Db, Options, Template, render};

let options = Options::new("orders-api", Template::Service).db(Db::Pg);
let files = render(&options)?;
assert!(files.iter().any(|f| f.path == "migrations/0001_init.sql"));
assert!(files.iter().any(|f| f.path == ".github/workflows/ci.yml"));
# Ok::<(), id_effect_tool::NewError>(())
```

`new_project(&options, dest)` writes the files; `cli::run` is the whole command line.

## Templates

The templates live in `templates/` (`common/` is shared by all four). Files end in `.template` and a leading `_` stands for `.` (`_github/` → `.github/`), so formatters and `cargo package` leave them alone; `src/template.rs` embeds each one and a test checks that table against the directory. Placeholders: `{{name}}`, `{{crate_name}}`, `{{id_effect_version}}`, `{{kit_features}}`; line blocks `{{#if pg}}` … `{{/if}}` (flags `pg`, `otel` and the template names, `!` negates).

## Tier

`integration` (ADR 0010 Amendment 4); it publishes after `id_effect_kit`.

## Examples

`cargo run -p id_effect_tool --example 010_render` prints the files each template generates.
