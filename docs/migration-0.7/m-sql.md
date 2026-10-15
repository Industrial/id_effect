# m-sql: `id_effect_sql` on the 0.7 kernel, client and transactions are scoped Layers

0.7 moves `id_effect_sql` to the new kernel (`id_effect::next`). Every trait method returns a
`next::Effect<_, SqlError>` (no `R`), the capabilities live in the typed `next` environment, and
the v1 `ProviderBox` providers are replaced by `next::Layer`s. The client and transactions are
**scoped layers** (Effect.ts `Layer.scoped`): their resources are released when the layer's scope
closes, after the program, however it ends.

## What changed

| 0.6 | 0.7 |
|---|---|
| `SqlClient` methods → v1 `Effect<_, SqlError, ()>` | → `next::Effect<_, SqlError>`; new `close()` (default: no-op) |
| `SqlTransaction { commit, rollback }` | adds `query` and `execute`: **statements on the handle run inside the transaction** (on pg: on the transaction's connection; in 0.6 `PgSqlClient` statements always went to the pool, outside the transaction) |
| `SqlClientService(Arc<dyn SqlClient>)` in a v1 `Env` | `SqlClientService` (derefs to `dyn SqlClient`, `SqlClientService::new(c)`) in `SqlClientEnv = HCons<SqlClientService, HNil>`; `sql_client_env(c)` for `.provide_all` |
| — | `sql_client_layer::<E>(client)`: connect on build (fails the build with `E::from`), `close` when the scope closes |
| — | `provide_sql_client::<E>(client)`: `Layer::succeed`, owned by the caller |
| — | `query(sql, params)` / `execute(sql, params)` read the client from `R` |
| `with_transaction(client, f)` / `transaction_scope(client, f)` → v1 effect, rollback on failure only | → `next::Effect<A, E>` with `E: From<SqlError>`; commits on success, rolls back on failure, **defect, panic or interrupt** (`acquire_use_release`); `BEGIN` and `COMMIT` run uninterruptibly; a failed `COMMIT` is a typed error. `transaction_scope` stays as an alias |
| — | `transaction(f)`: the same bracket, reading the client from `R` |
| — | `transaction_layer::<E>()`: `Layer<SqlTransactionEnv, E, SqlClientEnv>` (`Layer::acquire_release`): `BEGIN` on build; on scope close `COMMIT` when the owner succeeded, `ROLLBACK` on failure, defect or interrupt (reads the release's `ExitKind`). A failed `COMMIT` there cannot be a typed error (a release is `Effect<(), Never>`), so it is a **defect** of the owner: use the brackets when the commit error must be typed |
| `pg::PgPool = Arc<sqlx::PgPool>`, `provide_pg_pool(pool) -> ProviderBox` | `pg::PgPool(pub sqlx::PgPool)` (derefs to `sqlx::PgPool`), `PgPoolEnv`; `provide_pg_pool::<E>(pool) -> Layer<PgPoolEnv, E>` |
| `provide_pg_sql_client(pool) -> ProviderBox` (`SqlClientService` + `PgPool`) | `provide_pg_sql_client::<E>(pool) -> Layer<PgSqlEnv, E>`, `PgSqlEnv = HCons<SqlClientService, HCons<PgPool, HNil>>` (caller owns the pool) |
| — | `pg::PgSqlClient::layer::<E>(PgPoolConfig)`: opens the pool and checks it (`SELECT 1`) on build, **closes the pool** when the scope closes |
| `PgSqlTransaction` over a `PoolConnection` + manual `BEGIN` | over a sqlx `Transaction` (`pool.begin()`): a handle dropped unfinished is rolled back by sqlx |
| `TestSqlClient` rollback cleared every recorded statement | a transaction's statements are recorded when it commits and discarded when it rolls back; new `commits()`, `rollbacks()`, `closes()`, `is_connected()`, `recorded_executes()`, `fail_commits()` |
| dev-dependency on axum / `id_effect_axum`, `examples/010_axum_sql.rs` | removed (it ran against the publish order). Its axum + SQL wiring now lives in the showcase `examples/http-showcase` (`src/repo.rs`: a `TodoRepo` on the pool's `SqlClientService`, batches in `with_transaction`) |

Unchanged: `SqlError`, `SqlParam`, `SqlRow`, `PgSqlClient::{new, pool}`, `PgPoolConfig`,
`pg_pool_from_config`, `pg_pool_from_config_lazy`, `PgSqlError`. The live implementations need a
tokio runtime with its IO driver (sqlx): run them with `id_effect_tokio::TokioRuntime`. The test
double runs anywhere, `block_on` included.

## Before / after

```rust
// 0.6
let pool = pg_pool_from_config(PgPoolConfig::from_url(url)).await?;
let env = build_env([provide_pg_sql_client(pool)])?;
let db: Arc<dyn SqlClient> = Arc::new(PgSqlClient::new(pool.clone()));
let n = run_async(
  with_transaction(db, |tx| /* statements on the pool, not on tx */ ..),
  (),
).await?;

// 0.7
let insert: Effect<u64, AppError, SqlTransactionEnv> = need::<SqlTransactionService>()
  .flat_map(|tx| tx.execute("INSERT INTO todos (title) VALUES ($1)", &[SqlParam::Text(t)]).map_err(AppError::from).narrow());
let program = insert
  .provide_layer(transaction_layer())                 // BEGIN … COMMIT / ROLLBACK
  .provide_layer(PgSqlClient::layer(PgPoolConfig::from_url(url))); // open pool … close pool
let exit = TokioRuntime::current()?.run(program).await;

// or, as a bracket with a typed commit error:
let n = transaction(|tx| tx.execute(sql, &params).map_err(AppError::from).narrow());
```

Dependents: `id_effect_node` (`pg`) and the labs crates (`events`, `workflow`, `jobs`) only use
the kernel-neutral `PgPoolConfig` / `pg_pool_from_config` / `sqlx::PgPool`, so they needed no
compat adapters.
