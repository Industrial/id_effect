# Data access

[`id_effect_sql`](../../../id_effect_sql) provides driver-agnostic SQL as `Effect` values — the Rust analogue of `@effect/sql`.

## Core traits

- [`SqlClient`](https://docs.rs/id_effect_sql/latest/id_effect_sql/trait.SqlClient.html) — `connect`, `query`, `execute`, `begin`
- [`with_transaction`](https://docs.rs/id_effect_sql/latest/id_effect_sql/fn.with_transaction.html) — commit/rollback scope
- [`TestSqlClient`](https://docs.rs/id_effect_sql/latest/id_effect_sql/struct.TestSqlClient.html) — scriptable in-memory double

PostgreSQL production access lives in [`id_effect_sql_pg`](../../../id_effect_sql_pg). Driver choice ADR: [docs/platform/adrs/adr-sql-driver-choice.md](../../../../docs/platform/adrs/adr-sql-driver-choice.md).

## Axum integration

Run queries inside `id_effect_axum::routing` handlers with shared `Arc<dyn SqlClient>` state:

```bash
cargo run -p id_effect_sql --example 010_axum_sql
```

Keep SQL in context modules; inject `SqlClient` via capability providers at the host boundary (Part II).

## See also

- Mission: `platform-data`
