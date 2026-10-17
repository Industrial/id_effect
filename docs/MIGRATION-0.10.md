# Migrating to id_effect 0.10

This guide is the "if you used X, now do Y" companion to the `0.10.0` section of
[CHANGELOG.md](../CHANGELOG.md). It assumes you are on 0.9. If you are on 0.8, apply
[MIGRATION-0.9.md](MIGRATION-0.9.md) first.

Every intentional public-API break in 0.10 is listed here. A break reported by
`cargo semver-checks` (the `semver` CI job, baseline 0.9.0) that is not listed below is a bug.
The deprecation rule and semver policy are in [STABILITY.md](STABILITY.md).

Each 0.10 change appends its entry to the matching section below.

## Data: sqlx's `time` feature is off again

- `id_effect_sql`'s `pg`, `mysql` and `sqlite` features no longer enable sqlx's `time` feature,
  and the crate no longer depends on `time` (this reverses the 0.9 section "sqlx's `time` feature
  is on"). Date-times are converted by `id_effect_sql` itself, from each database's wire format
  into jiff; the public API (`SqlValue::Timestamp` / `DateTime` / `Date` / `Time`) is unchanged.
  With sqlx's `chrono` in your build, sqlx's `query!` / `query_as!` macros type date-time columns
  as chrono types again, and dependencies written against that mapping compile again. Code of
  your own that decodes sqlx rows into `time` types (`OffsetDateTime`, `PrimitiveDateTime`) must
  now enable sqlx's `time` feature itself (`sqlx = { version = "0.8", features = ["time"] }`).
  CI fails if `id_effect_sql` or `id_effect_kit` brings either back.
- The `query` feature enables sea-query's `with-jiff` only (not `with-time`). Build statements
  with jiff values (`Expr::val(jiff::Timestamp)`, or `Expr::val(x.to_sql_value())`); a
  `sea_query::Value::TimeDateTime*` / `TimeDate` / `TimeTime` value, which now exists only when
  another crate turns on sea-query's `with-time`, is `SqlError::Unsupported` in `sql_values` and
  `SqlValue::try_from`.
- PostgreSQL: `TIMESTAMPTZ` / `TIMESTAMP` / `DATE` / `TIME` values that jiff cannot hold
  (`infinity`, `-infinity`, years beyond ±9999) and values before PostgreSQL's earliest date
  (4714-11-24 BC) are `SqlError::ValueOutOfRange`, never a panic.
- MySQL: `DATE`, `DATETIME` and `TIMESTAMP` parameters are sent as MySQL literals
  (`'2024-02-29 12:34:56.123456'`, a `Timestamp` as its UTC wall clock) instead of typed
  date-time parameters; the server converts them when it stores or compares them, so inserts,
  updates and `WHERE` clauses behave as in 0.9. A parameter with no column to convert to (a bare
  `SELECT ?`, `COALESCE(?, …)`) now reads back as `SqlValue::Text`: write
  `CAST(? AS DATETIME(6))` (or `DATE`) where the type matters. `TIME` parameters are unchanged.
