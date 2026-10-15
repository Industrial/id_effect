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

Routes: `GET /health` → `200 ok`.

## Run

```sh
devenv up            # PostgreSQL on 127.0.0.1:5432 (database `id_effect`), in another terminal
devenv shell         # sets DATABASE_URL=postgresql://postgres@127.0.0.1:5432/id_effect
cargo run -p http_showcase
curl -i localhost:8080/health
```

Without `DATABASE_URL` (or with it empty) the service starts without a database.

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
```
