# `id_effect_sql`

Driver-agnostic **SQL client** capability for [`id_effect`](../id_effect), aligned with Effect.ts [`@effect/sql`](https://effect.website/docs/sql/introduction), on the 0.7 kernel.

Application code reads the `SqlClientService` capability (`SqlClientEnv`) from its environment; production wires the PostgreSQL driver behind the `pg` feature, tests use the scriptable in-memory `TestSqlClient`. The client and transactions are **scoped layers**: their resources are released when the layer's scope closes, however the program ends.

## Quick start

```rust
use id_effect::Exit;
use id_effect::{Effect, need};
use id_effect_sql::{
  SqlClientEnv, SqlError, SqlRow, SqlTransactionEnv, SqlTransactionService, TestSqlClient, params,
  query, sql_client_layer, transaction_layer,
};

/// Domain code depends only on the capability.
fn count_users() -> Effect<usize, SqlError, SqlClientEnv> {
  query("SELECT id FROM users WHERE active = $1", params![true])
    .map(|rows| rows.len())
}

/// Runs inside a transaction: committed on success, rolled back on failure or interrupt.
fn deactivate_all() -> Effect<u64, SqlError, SqlTransactionEnv> {
  need::<SqlTransactionService>()
    .flat_map(|tx| tx.execute("UPDATE users SET active = false", &[]).narrow())
}

fn main() {
  let test = TestSqlClient::new();
  test.script_query(
    "SELECT id FROM users WHERE active = $1",
    vec![SqlRow::from_pairs([("id", 1i64)]), SqlRow::from_pairs([("id", 2i64)])],
  );

  let program = count_users()
    .flat_map(|n| deactivate_all().provide_layer(transaction_layer()).map(move |_| n))
    .provide_layer(sql_client_layer::<SqlError>(test.clone())); // connect … close
  assert!(matches!(program.block_on(), Exit::Success(2)));
  assert_eq!((test.commits(), test.closes()), (1, 1));
}
```

Parameters are typed `SqlValue`s (`params![id, title]` encodes Rust values; `None` binds NULL), and rows are typed: `row.get::<i64>("id")` / `row.get::<String>(1)` decode a column (`uuid`, `jiff` dates and times, `rust_decimal`, `serde_json` included) and return a `SqlError` naming the column instead of panicking.

Brackets are available too: `with_transaction(client, |tx| ..)` and `transaction(|tx| ..)` (which reads the client from the environment) commit on success and roll back on failure, defect or interrupt; a failed `COMMIT` is a typed error there.

With the `pg` feature, swap `sql_client_layer(test)` for `id_effect_sql::pg::PgSqlClient::layer(PgPoolConfig::from_url(url))`, which opens a pool when built and closes it afterwards, and provides the `PgPool` too. Its effects run on tokio (`id_effect_tokio::TokioRuntime`).

## Modules

| Module | Description |
|--------|-------------|
| [`error`](src/error.rs) | `SqlError` |
| [`client`](src/client.rs) | `SqlClient`, `SqlClientService`, `sql_client_layer`, `provide_sql_client`, `query`, `execute`, `TestSqlClient` |
| [`value`](src/value/mod.rs) | `SqlValue`, `SqlType`, `ToSqlValue`, `FromSqlValue`, `ValueError`, `Json<T>`, `params!` |
| [`row`](src/row.rs) | `SqlRow` (`get`, `value`, `from_pairs`), `SqlColumn`, `ColumnIndex` |
| [`transaction`](src/transaction.rs) | `SqlTransaction`, `SqlTransactionService`, `transaction_layer`, `with_transaction` (alias `transaction_scope`), `transaction` |
| [`pg`](src/pg/mod.rs) (feature `pg`) | `PgSqlClient` (+ `PgSqlClient::layer`), `PgPool`, `PgPoolConfig`, `pg_pool_from_config`, `provide_pg_sql_client`, `provide_pg_pool` |

## Design

See ADR [`adr-sql-driver-choice.md`](../../docs/platform/adrs/adr-sql-driver-choice.md) and the spec [`phase-c-sql.md`](../../docs/effect-ts-parity/phases/phase-c-sql.md).

## Testing

```bash
cargo nextest run -p id_effect_sql --all-features
```

Live PostgreSQL tests (`tests/pg.rs`, `live_postgres_*`) read `DATABASE_URL`. Without a database they print
`SKIP …` to stderr; with `ID_EFFECT_REQUIRE_PG=1` (set by CI whenever it provides a Postgres service) a
missing or unreachable database **fails** the run instead.

```bash
devenv up -d   # postgres (devenv.nix)
devenv shell -- env ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_sql --features pg
```
