# DX and deploy

The `id-effect` command (crate `id_effect_tool`) starts a project and checks that it can run;
every generated project ships with its build, CI and deploy files.
[Tutorial: zero to production](../tutorial-zero-to-production.md) walks one service through all of
it.

## Starting an app: `id-effect new`

```sh
cargo install id_effect_tool
id-effect new orders-api --template service      # or worker, cli, cluster
```

| Template | What you get |
|---|---|
| `service` | An Axum app with `SecurityLayer::recommended`, request ids, compression, a timeout and a body limit, a typed `GET /hello/{name}` route answering bad input with a 422 Problem, `/health`, `/ready`, `/metrics`, graceful shutdown. `--db pg` adds a PostgreSQL pool, `migrations/` applied on start by `id_effect_sql::migrate`, a `db` readiness check and a CI job running `id-effect migrate check`; `--otel` OTLP export. |
| `worker` | Named cron jobs in a `CronRegistry` under `run_main`. |
| `cli` | clap, `run_main` and shell completions (the [`examples/cli-minimal`](https://github.com/Industrial/id_effect/tree/main/examples/cli-minimal) shape). |
| `cluster` | An `id_effect_node` mesh node hosting a process behavior. |

With `--db pg`, each migration runs once, in its own transaction, under an advisory lock so
replicas starting together don't race, and is recorded in `_id_effect_migrations`; a migration
edited after it ran stops the service from starting. Add the next one with
`id-effect migrate new <name>` (see [Migrations](./ch28-02-migrations.md)).

Each project depends on `id_effect_kit` (one dependency, a feature per battery: `web`, `openapi`,
`sql-pg`, `migrate`, `repository`, `auth`, `authz`, `redis`, …; `test` belongs on a
`[dev-dependencies]` entry, so containers and fixtures never reach the shipped binary) and has `src/`,
`tests/`, a multi-stage `Dockerfile`, `compose.yaml`, `.github/workflows/ci.yml` (fmt, clippy,
test, docker build), `.env.example` and a README. `--local-workspace PATH` builds against an
id_effect checkout instead of crates.io. The [crate README](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_tool/README.md)
lists every option.

## Checking a project: `id-effect doctor`

`id-effect doctor` checks the toolchain, that the id_effect crates resolve to one version, the
`.env.example` keys, the database, docker and the port, and exits `1` when a check fails;
`--graph` also prints the app's layer graph. See [Doctor and the layer graph](./ch34-01-doctor-graph.md).

## Deploy

The `service` template's `config/deploy.yml` is a [Kamal 2](https://kamal-deploy.org) config
(service, image, servers, registry, the `/health` check of Kamal's proxy, the environment and
secrets); the `Dockerfile` builds the release binary into `debian:bookworm-slim`. Fill in the
image, servers and registry, put the secrets in `.kamal/secrets`, then `kamal setup` once and
`kamal deploy` per release.

## See also

- Mission: `platform-dx-ship`
- [ROADMAP.md](https://github.com/Industrial/id_effect/blob/main/docs/platform/ROADMAP.md)
