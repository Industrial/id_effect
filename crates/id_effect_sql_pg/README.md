# `id_effect_sql_pg`

PostgreSQL driver for [`id_effect_sql`](../id_effect_sql) — sqlx `PgPool` and `PgSqlClient` wired into the id_effect SQL client traits.

## Features

- `PgSqlClient` implementing the driver-agnostic `SqlClient` trait from `id_effect_sql`
- Connection pooling via sqlx `PgPool`
- Transaction support through `id_effect_sql::with_transaction`

## Usage

Add both `id_effect_sql` and this crate, then provide a `PgSqlClient` backed by a shared `PgPool` in your layer graph.

See [`id_effect_sql`](../id_effect_sql) for trait documentation and [`docs/platform/adrs/adr-sql-driver-choice.md`](../../docs/platform/adrs/adr-sql-driver-choice.md) for driver selection rationale.
