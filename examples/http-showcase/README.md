# http_showcase

The id_effect 0.7 showcase (not published): an axum HTTP service whose whole lifetime is one
effect on the fiber runtime.

- `AppConfig` is one `Config<T>` descriptor (`src/config.rs`), loaded by its layer
  (`AppConfig::layer()`) from the process environment and provided as a capability.
- The HTTP server is a scoped layer (`id_effect_axum::server::serve_until`): it binds on acquire and
  drains gracefully on release (SIGINT / SIGTERM).
- With `DATABASE_URL` set, a scoped `PgSqlClient` pool layer (`id_effect_sql`, `pg` feature) is
  opened (and checked with `SELECT 1`) before serving and closed after.
- Every request runs as one interruptible fiber on tokio.
- The todo store is a capability (`TodoRepoService`, `src/repo.rs`): `PgTodoRepo` on the pool
  (the schema `migrations/0001_init.sql` is applied at startup), or `InMemoryTodoRepo` without
  `DATABASE_URL` (and in the tests).
- Each store call runs under the `RequestPolicy` (`src/app.rs`): a transient failure (store
  unavailable) is retried twice with exponential backoff (`Effect::retry` + `Schedule`), and the
  whole call is bounded by `REQUEST_TIMEOUT` (`timeout_fail`, answered `504`).

## Endpoints

| Method and path | Body | Success |
|---|---|---|
| `GET /health` | | `200 ok` |
| `GET /todos` | | `200` `[Todo]` |
| `POST /todos` | `{"title": "milk"}` | `201` `Todo` |
| `POST /todos/batch` | `{"titles": ["eggs", "bread"]}` | `201` `[Todo]`: all in one transaction, or none |
| `GET /todos/{id}` | | `200` `Todo` |
| `PATCH /todos/{id}` | `{"title": "..", "completed": true}` (either field) | `200` `Todo` |
| `DELETE /todos/{id}` | | `204` |

A `Todo` is `{"id": 1, "title": "milk", "completed": false}`. Titles are trimmed, 1 to 200
characters, and unique.

Errors (`AppError`, `src/error.rs`) answer `{"error": "<code>", "message": "..."}`:

| Status | `error` | When |
|---|---|---|
| `404` | `not_found` | no todo with that id |
| `409` | `conflict` | the title is taken (a conflicting batch stores nothing) |
| `422` | `invalid` | a bad id, JSON body or title |
| `503` | `unavailable` | the store is still unreachable after the retries |
| `504` | `timeout` | the request ran past `REQUEST_TIMEOUT` |
| `500` | `internal` | any other store failure (details stay in the server) |

```sh
curl -i -X POST localhost:8080/todos -H 'content-type: application/json' -d '{"title":"milk"}'
curl -i localhost:8080/todos
curl -i -X PATCH localhost:8080/todos/1 -H 'content-type: application/json' -d '{"completed":true}'
curl -i -X DELETE localhost:8080/todos/1
```

## Run

```sh
devenv up            # PostgreSQL on 127.0.0.1:5432 (database `id_effect`), in another terminal
devenv shell         # sets DATABASE_URL=postgresql://postgres@127.0.0.1:5432/id_effect
cargo run -p http_showcase
curl -i localhost:8080/health
```

Without `DATABASE_URL` (or with it empty) the service starts without a database and keeps the
todos in memory.

| Variable | Default | Meaning |
|---|---|---|
| `HOST` | `0.0.0.0` | bind address |
| `PORT` | `8080` | listen port |
| `SHUTDOWN_TIMEOUT_SECS` | `30` | graceful drain on shutdown |
| `REQUEST_TIMEOUT` | `2s` | per-request handler timeout (`500ms`, `2s`, ...) |
| `DATABASE_URL` | unset | PostgreSQL URL; unset runs without a database |
| `DATABASE_MAX_CONNECTIONS` | `16` | pool size when `DATABASE_URL` is set |

All keys are read together, so a bad configuration reports every invalid key at once.

## The program

```rust,no_run
use id_effect::Exit;
use id_effect_axum::EffectExecutor;

# async fn run() {
// env provider → AppConfig layer → server (+ pool) layers → serve until SIGINT / SIGTERM
let program = http_showcase::run_until_shutdown().provide_layer(http_showcase::live_layer());
match EffectExecutor::global().run_effect(program).await {
  Exit::Success(reason) => println!("stopped: {reason:?}"),
  Exit::Failure(cause) => eprintln!("failed: {cause:?}"),
}
# }
```

## Test

```sh
cargo nextest run -p http_showcase
# the live PostgreSQL tests (CRUD, a failed batch rolls back) need the database:
devenv shell -- env ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p http_showcase
```

Without `DATABASE_URL` the live tests print `SKIP`; with `ID_EFFECT_REQUIRE_PG=1` a missing
database fails them.
