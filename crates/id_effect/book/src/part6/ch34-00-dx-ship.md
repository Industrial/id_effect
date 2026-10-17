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
(service, image, servers, registry, the `/ready` check of Kamal's proxy, the drain timeout, the
environment and secrets); the `Dockerfile` builds the release binary into `debian:bookworm-slim`.
Fill in the image, servers and registry, put the secrets in `.kamal/secrets`, then `kamal setup`
once and `kamal deploy` per release.

### Rolling restarts and drain

A deploy replaces instances one at a time without dropping requests. A generated service does this
out of the box: its `main` runs `HostBuilder::run_until_shutdown` with the project's `serve`
(`serve_router` over the app wrapped in `track_in_flight(app, &host.drain)`, so in-flight requests
are counted), its `HealthRegistry` built `with_readiness(host.readiness.clone())`. On SIGTERM it
marks its readiness shutting down, so `/ready` answers `503` (`"shutting_down"`) while the listener
keeps accepting and in-flight requests finish, for at most `SHUTDOWN_TIMEOUT_SECS` (default `30`);
then the process exits. A hand-written host gets the same from those pieces.
(`serve_until_shutdown` and `server_layer_with_readiness` stop accepting at once; the latter marks
a shared `ReadinessState` the same way.) `/health` stays `200` throughout: the process is alive,
just not taking new work. The sequence:

1. The new instance starts and its `/ready` turns `200`.
2. The old instance gets SIGTERM; its `/ready` turns `503`.
3. The proxy or load balancer stops routing new requests to it.
4. Its in-flight requests finish (or `SHUTDOWN_TIMEOUT_SECS` runs out).
5. It exits.

`on_sigterm_ready_turns_503_while_an_in_flight_request_completes`
(`crates/id_effect_axum/tests/health.rs:216`) is the evidence: it sends the process SIGTERM with a
request in flight, sees `/ready` answer `503` while that request is still open, then sees the
request complete and the server exit. A generated project's `tests/shutdown.rs` runs the same check
against its own `app` and `serve`.

**Kamal.** The generated `config/deploy.yml` carries the settings:

```yaml
drain_timeout: 35

proxy:
  app_port: 8080
  healthcheck:
    path: /ready
    interval: 1
    timeout: 5

env:
  clear:
    SHUTDOWN_TIMEOUT_SECS: "30"
```

`kamal deploy` boots the new container and kamal-proxy polls its `/ready` until it answers `200`
(a `--db pg` service only does once the database check passes); the proxy then sends new requests
there, waits up to `drain_timeout` seconds for the old container's in-flight requests, and stops
it, which sends SIGTERM and kills it `drain_timeout` seconds later. The proxy has already moved
traffic by the time SIGTERM arrives, so here the `503` only matters to other probes. Keep
`drain_timeout` above `SHUTDOWN_TIMEOUT_SECS` so the service's own drain finishes before the
kill. Run more than one server under `servers.web` (behind a load balancer) for the deploy to
roll across hosts.

**Kubernetes.** The same settings, as a Deployment:

```yaml
spec:
  strategy:
    type: RollingUpdate
    rollingUpdate:
      maxUnavailable: 0      # never fewer ready pods than replicas
      maxSurge: 1            # one new pod at a time
  template:
    spec:
      terminationGracePeriodSeconds: 45   # > preStop sleep + SHUTDOWN_TIMEOUT_SECS
      containers:
        - name: app
          env:
            - name: SHUTDOWN_TIMEOUT_SECS
              value: "30"
          readinessProbe:
            httpGet: { path: /ready, port: 8080 }
            periodSeconds: 2
            failureThreshold: 1
          livenessProbe:
            httpGet: { path: /health, port: 8080 }
          lifecycle:
            preStop:
              sleep: { seconds: 5 }
```

Kubernetes removes a terminating pod from its Service endpoints at the same time as it starts the
`preStop` hook, so the hook's short sleep lets kube-proxy and ingress controllers catch up before
SIGTERM reaches the service. The readiness probe on `/ready` holds the rollout until the new pod is
ready and drops a draining pod from the endpoints; the liveness probe stays on `/health`, so a
draining pod is never restarted. `terminationGracePeriodSeconds` must cover the `preStop` sleep
plus `SHUTDOWN_TIMEOUT_SECS`, or the kubelet's SIGKILL cuts the drain short.

## See also

- Mission: `platform-dx-ship`
- [ROADMAP.md](https://github.com/Industrial/id_effect/blob/main/docs/platform/ROADMAP.md)
