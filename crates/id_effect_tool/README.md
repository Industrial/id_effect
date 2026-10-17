# `id_effect_tool`

The `id-effect` command. `id-effect new` generates a ready-to-run project on [`id_effect_kit`](../id_effect_kit) from one of four templates embedded in the binary, so it works the same after `cargo install`. `id-effect doctor` checks that a project can build and run on this machine.

```sh
cargo install id_effect_tool          # installs the `id-effect` binary
id-effect new orders-api --template service
cd orders-api && cargo test && cargo run
id-effect doctor                      # toolchain, versions, env, database, docker, port
id-effect migrate run                 # apply migrations/ to DATABASE_URL
```

The book's [zero-to-production tutorial](../id_effect/book/src/tutorial-zero-to-production.md) takes a generated service through a route, a test, PostgreSQL, doctor, the Docker image and the deploy config.

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

## `id-effect doctor`

```text
id-effect doctor [--path DIR] [--json] [--graph[=text|dot]]
```

Runs every check on the project in `DIR` (default: the current directory) and prints one line per check, `pass`, `warn`, `fail` or `skip`, with the reason: `project` (a `Cargo.toml`), `rustc` (against the project's `rust-version` and the id_effect MSRV), `versions` (the id_effect crates in `Cargo.lock` resolve to one version), `env` (every `.env.example` key set in the environment or `.env`), `database` (`DATABASE_URL` accepts a TCP connection), `docker` (the CLIs, when a `Dockerfile` or compose file exists) and `port` (`HOST:PORT` is free). `--graph` also builds the project, runs it with `--describe-layers` and prints its layer graph (`id_effect_kit::graph::LayerGraph`), which the `service` template registers. It exits `1` when a check fails, else `0`; `--json` prints the report as one object. The book chapter [Doctor and the layer graph](../id_effect/book/src/part6/ch34-01-doctor-graph.md) has the full table.

## `id-effect migrate`

```text
id-effect migrate new <NAME> [--reversible] [--timestamp | --sequential] [--dir DIR]
id-effect migrate run    [DB OPTIONS]
id-effect migrate status [DB OPTIONS]
id-effect migrate revert [--count N] [DB OPTIONS]
id-effect migrate check  [DB OPTIONS]

DB OPTIONS: [--dir DIR] [--database-url URL] [--table TABLE] [--lock-timeout SECS] [--json]
```

Versioned SQL migrations on PostgreSQL, run by the engine of `id_effect_sql::migrate` (the same one `migrate::layer` runs at startup): a tracking table (`_id_effect_migrations`, or `--table`), an advisory lock so concurrent migrators apply each migration once, and one transaction per migration. Files are sqlx-cli's, in `--dir` (default `migrations`): `<VERSION>_<NAME>.sql`, or `<VERSION>_<NAME>.up.sql` plus `.down.sql` for a reversible migration.

- `new` writes the next migration (`--reversible`: the up/down pair). It numbers after the directory: short sequential versions such as the `service` template's `0001_init.sql` continue at their width (`0002_add_users.sql`); an empty directory, or one of timestamps, gets the UTC time `YYYYMMDDHHMMSS` like sqlx-cli (bumped past the newest version if that is not later). `--timestamp` / `--sequential` force a scheme.
- `run` applies every pending migration, oldest first, and names each script it applied.
- `status` lists each migration as `applied`, `pending`, `drifted` (its script changed since it was applied) or `missing` (applied, but no longer in `DIR`), with when it was applied. It always exits `0`.
- `revert` runs the down scripts of the last `--count` (default 1) applied migrations, newest first.
- `check` is the CI gate: it exits `1` when a migration is pending, drifted or missing.

The database is `--database-url`, else `DATABASE_URL` from the environment, else from `./.env`; the URL is never printed. `--json` prints one object, `{"ok": true, …}` or, on failure, `{"ok": false, "error": {"kind", "message", "version"?, "file"?, "versions"?}}` on stdout.

| Exit | Meaning |
|---|---|
| `0` | done (`check`: up to date) |
| `1` | the migrations need attention: pending (`check`), drift (any command but `status`), a failing script (rolled back, the run stops), no down script to revert, an unreadable `DIR`, or `new` could not write |
| `2` | usage: bad arguments, no database URL, not a `postgres://` URL |
| `3` | the database: cannot connect, the migration lock stayed taken past `--lock-timeout`, a bookkeeping query failed |

```sh
id-effect migrate new add_users --reversible   # migrations/0002_add_users.{up,down}.sql
id-effect migrate run                          # applied  0002_add_users.up.sql
id-effect migrate check                        # in CI: exit 1 on pending or changed migrations
```

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
