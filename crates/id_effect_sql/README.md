# `id_effect_sql`

Driver-agnostic **SQL client** traits for [`id_effect`](../id_effect), aligned with Effect.ts [`@effect/sql`](https://effect.website/docs/sql/introduction).

## Modules

| Module | Description |
|--------|-------------|
| [`error`](src/error.rs) | `SqlError` |
| [`client`](src/client.rs) | `SqlClient`, `SqlRow`, `SqlParam`, `TestSqlClient` |
| [`transaction`](src/transaction.rs) | `SqlTransaction`, `with_transaction` transaction scope |

## Design

See ADR [`adr-sql-driver-choice.md`](../../docs/platform/adrs/adr-sql-driver-choice.md) and Phase C spec [`phase-c-sql.md`](../../docs/effect-ts-parity/phases/phase-c-sql.md).

Production PostgreSQL support lives in `id_effect_sql_pg` (sqlx `PgPool`).

## Testing

```bash
cargo nextest run -p id_effect_sql
```
