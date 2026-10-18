# Data access

[`id_effect_sql`](https://docs.rs/id_effect_sql) provides driver-agnostic SQL as `Effect` values — the Rust analogue of `@effect/sql`.

## Core API

- [`SqlClient`](https://docs.rs/id_effect_sql/latest/id_effect_sql/trait.SqlClient.html) — `connect`, `query`, `execute`, `begin`, `close`; every method returns `Effect<_, SqlError>`
- `SqlClientService` in `SqlClientEnv = HCons<SqlClientService, HNil>` — the client as a capability; `query(sql, params)` / `execute(sql, params)` read it from `R`
- `sql_client_layer::<E>(client)` — a **scoped layer**: connects on build, `close` when the scope closes; `provide_sql_client::<E>(client)` for a caller-owned client; `sql_client_env(client)` for `.provide_all`
- `with_transaction(client, f)` / `transaction(f)` — a bracket: commits on success, rolls back on failure, defect or interrupt; a failed `COMMIT` is a typed error
- `transaction_layer::<E>()` — `BEGIN` on build, `COMMIT` / `ROLLBACK` when the owner's scope closes
- [`TestSqlClient`](https://docs.rs/id_effect_sql/latest/id_effect_sql/struct.TestSqlClient.html) — scriptable in-memory double (`commits()`, `rollbacks()`, `recorded_executes()`, …); runs anywhere, `block_on` included
- Parameters are typed `SqlValue`s (`params![id, title]`) and rows decode by column with `row.get::<T>("id")`, returning a `SqlError` that names the column: see [Typed SQL values and rows](./ch28-01-typed-sql.md)
- Versioned schema migrations (`migrate` feature): a startup layer, the `id-effect migrate` command and a CI `check`; see [Migrations](./ch28-02-migrations.md)

PostgreSQL lives in [`id_effect_sql::pg`](https://docs.rs/id_effect_sql/latest/id_effect_sql/pg/index.html) behind the `pg` feature: `PgPool`, `PgSqlClient` (sqlx-backed), `PgSqlClient::layer::<E>(PgPoolConfig)` (opens and checks the pool on build, closes it with the scope) and `provide_pg_sql_client::<E>(pool)` for a caller-owned pool.

```rust,no_run
use id_effect::{Effect, need};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient};
use id_effect_sql::{SqlTransactionEnv, SqlTransactionService, params, transaction_layer};
use id_effect_tokio::TokioRuntime;

# #[derive(Debug)]
# struct AppError(id_effect_sql::SqlError);
# impl From<id_effect_sql::SqlError> for AppError {
#     fn from(e: id_effect_sql::SqlError) -> Self { AppError(e) }
# }
# async fn insert_todo(title: String, url: String) -> std::io::Result<()> {
let insert: Effect<u64, AppError, SqlTransactionEnv> = need::<SqlTransactionService>().flat_map(move |tx| {
    tx.execute("INSERT INTO todos (title) VALUES ($1)", &params![title])
        .map_err(AppError::from)
        .narrow()
});
let program = insert
    .provide_layer(transaction_layer())                               // BEGIN … COMMIT / ROLLBACK
    .provide_layer(PgSqlClient::layer(PgPoolConfig::from_url(url))); // open pool … close pool
let exit = TokioRuntime::current()?.run(program).await;
# let _ = exit;
# Ok(())
# }
# fn main() {}
```

The live client needs a tokio runtime with its IO driver (sqlx): run it with `id_effect_tokio::TokioRuntime`.

```toml
id_effect_sql = { version = "0.10", features = ["pg"] }
```

Live Postgres tests read `DATABASE_URL`. They print `SKIP` when no database is reachable, and fail instead when `ID_EFFECT_REQUIRE_PG=1` (set by CI's `pg-integration` job). Driver choice ADR: [docs/platform/adrs/adr-sql-driver-choice.md](https://github.com/Industrial/id_effect/blob/main/docs/platform/adrs/adr-sql-driver-choice.md).

## Axum integration

The showcase [`examples/http-showcase`](https://github.com/Industrial/id_effect/tree/main/examples/http-showcase) wires axum and SQL together (`src/infrastructure/postgres/pg_todo_repository.rs`: a repository on the pool's `SqlClientService`, batches in `with_transaction`). Keep SQL in context modules; provide the client as a layer at the host boundary (Part II).

## See also

- Mission: `platform-data`
