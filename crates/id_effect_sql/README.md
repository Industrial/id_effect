# `id_effect_sql`

Driver-agnostic **SQL client** capability for [`id_effect`](../id_effect), aligned with Effect.ts [`@effect/sql`](https://effect.website/docs/sql/introduction).

Application code talks to `Arc<dyn SqlClient>` (or the `SqlClientService` capability); production wires the PostgreSQL driver behind the `pg` feature, tests use the scriptable in-memory `TestSqlClient`.

## Quick start

```rust
use std::sync::Arc;

use id_effect::{Effect, run_blocking};
use id_effect_sql::{SqlClient, SqlError, SqlParam, SqlRow, TestSqlClient, transaction_scope};

/// Domain code depends only on the trait.
fn count_users(db: Arc<dyn SqlClient>) -> Effect<usize, SqlError, ()> {
  db.query("SELECT id FROM users WHERE active = $1", &[SqlParam::Bool(true)])
    .map(|rows| rows.len())
}

fn main() {
  let test = TestSqlClient::new();
  test.script_query(
    "SELECT id FROM users WHERE active = $1",
    vec![SqlRow::new(vec!["1".into()]), SqlRow::new(vec!["2".into()])],
  );
  let db: Arc<dyn SqlClient> = Arc::new(test.clone());
  run_blocking(db.connect(), ()).unwrap();

  assert_eq!(run_blocking(count_users(db.clone()), ()), Ok(2));

  // Commit on success, rollback on failure.
  let n = run_blocking(transaction_scope(db, |_tx| count_users_in_tx()), ()).unwrap();
  assert_eq!(n, 0);
  assert_eq!(test.open_transactions(), 0);
}

fn count_users_in_tx() -> Effect<u32, SqlError, ()> {
  Effect::new(|_r: &mut ()| Ok(0))
}
```

With the `pg` feature, swap `TestSqlClient` for `id_effect_sql::pg::PgSqlClient::new(pool)` (pool from
`pg_pool_from_config`) or register `provide_pg_sql_client(pool)` in a capability `Env`.

## Modules

| Module | Description |
|--------|-------------|
| [`error`](src/error.rs) | `SqlError` |
| [`client`](src/client.rs) | `SqlClient`, `SqlRow`, `SqlParam`, `TestSqlClient`, `transaction_scope` |
| [`transaction`](src/transaction.rs) | `SqlTransaction`, `with_transaction` transaction scope |
| [`pg`](src/pg/mod.rs) (feature `pg`) | `PgSqlClient`, `PgPoolConfig`, `pg_pool_from_config`, `provide_pg_sql_client` (formerly `id_effect_sql_pg`) |

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
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres ID_EFFECT_REQUIRE_PG=1 \
  cargo nextest run -p id_effect_sql --features pg --test pg
```
