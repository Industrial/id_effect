# http_showcase

The id_effect 0.7 showcase (not published): an axum HTTP service whose whole lifetime is one
effect on the fiber runtime, laid out in domain-driven layers with one construct (struct, enum,
trait, type alias, function) per file.

## Layout

```
src/
  domain/todo/          Todo aggregate, TodoId, Title, TodoChanges; domain errors
                        (TodoNotFound, TitleTaken, InvalidTitle); the TodoRepository port,
                        its StoreError and its capability (TodoRepositoryService)
  application/          one use case per endpoint (list_todos, get_todo, create_todo, ...),
                        RequestPolicy, AppEnv
  infrastructure/
    memory/             InMemoryTodoRepository (+ its ProviderSpec layer)
    postgres/           PgTodoRepository, its layer (migrates on build), the pool layer,
                        Migration, row decoding, SqlError -> StoreError
    config/             AppConfig, DatabaseConfig (one Config<T> descriptor)
    telemetry/          one span and one latency observation per request
  interface/http/       Schema-decoded request bodies, handlers, router, error -> status
  bootstrap/            the layer graph: config -> server + store -> router; StartupError
```

- **Domain first.** `Todo` holds a validated `Title` and changes only through its behaviour
  (`rename`, `complete`, `reopen`, `apply`). The repository port fails only with `StoreError`:
  a missing todo or a taken title is part of its success value, so retries and the circuit
  breaker only ever see store failures.
- **Typed errors per use case.** Each use case says exactly how it fails, e.g.
  `update_todo: Effect<Todo, err!(InvalidTitle | TodoNotFound | TitleTaken | StoreFailure), AppEnv>`.
  Only `interface/http` turns errors into statuses (`IntoApiError`).
- **Resilient store calls.** Each store call runs under the `RequestPolicy`: a shared
  `CircuitBreaker` (`id_effect_resilience`, opens after 5 failed attempts, half-opens after 5 s),
  transient failures retried twice with exponential backoff (`Schedule`), the whole call bounded
  by `REQUEST_TIMEOUT` (`timeout_fail`, answered `504`).
- **Layers all the way.** `AppConfig` is one `Config<T>` descriptor provided by its layer. The
  store is a layer chosen by the config: `InMemoryTodoRepositoryLive` (`#[derive(ProviderSpec)]`)
  or the scoped `PgSqlClient` pool feeding `PgTodoRepositoryLive`, which applies
  `migrations/0001_init.sql` under an advisory lock when built. The HTTP server is a scoped layer:
  it binds on acquire, logs `level=INFO message="http_showcase listening" addr=.. store=..` on
  stderr (`id_effect_logger`) and drains gracefully on release (SIGINT / SIGTERM).
- Every request runs as one interruptible fiber on tokio. Request bodies are decoded with
  `#[derive(Schema)]`; unknown fields, bad JSON or a missing JSON content type answer `422`.

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

Errors (`ApiError`, `src/interface/http/`) answer `{"error": "<code>", "message": "..."}`:

| Status | `error` | When |
|---|---|---|
| `404` | `not_found` | no todo with that id |
| `409` | `conflict` | the title is taken (a conflicting batch stores nothing) |
| `422` | `invalid` | a bad id, JSON body or title |
| `503` | `unavailable` | the store is still unreachable after the retries, or the circuit breaker is open |
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
// env provider → AppConfig layer → server + store layers → serve until SIGINT / SIGTERM
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
