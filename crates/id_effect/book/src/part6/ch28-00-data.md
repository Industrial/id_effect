# Data access

[`id_effect_sql`](https://docs.rs/id_effect_sql) provides driver-agnostic SQL as `Effect` values — the Rust analogue of `@effect/sql`.

## Core traits

- [`SqlClient`](https://docs.rs/id_effect_sql/latest/id_effect_sql/trait.SqlClient.html) — `connect`, `query`, `execute`, `begin`
- [`with_transaction`](https://docs.rs/id_effect_sql/latest/id_effect_sql/fn.with_transaction.html) — commit/rollback scope
- [`TestSqlClient`](https://docs.rs/id_effect_sql/latest/id_effect_sql/struct.TestSqlClient.html) — scriptable in-memory double

PostgreSQL production access lives in [`id_effect_sql::pg`](https://docs.rs/id_effect_sql/latest/id_effect_sql/pg/index.html) behind the `pg` feature: `PgPool`, `PgSqlClient` (a sqlx-backed `SqlClient`) and `provide_pg_sql_client`. It was the separate `id_effect_sql_pg` crate before 0.6; that crate is now a deprecated re-export shim, removed in 0.7.

```toml
id_effect_sql = { version = "0.6", features = ["pg"] }
```

Live Postgres tests read `DATABASE_URL`. They print `SKIP` when no database is reachable, and fail instead when `ID_EFFECT_REQUIRE_PG=1` (set by CI's `pg-integration` job). Driver choice ADR: [docs/platform/adrs/adr-sql-driver-choice.md](https://github.com/Industrial/id_effect/blob/main/docs/platform/adrs/adr-sql-driver-choice.md).

## Axum integration

Run queries inside `id_effect_axum::routing` handlers with shared `Arc<dyn SqlClient>` state:

```bash
cargo run -p id_effect_sql --example 010_axum_sql
```

Keep SQL in context modules; inject `SqlClient` via capability providers at the host boundary (Part II).

## See also

- Mission: `platform-data`
