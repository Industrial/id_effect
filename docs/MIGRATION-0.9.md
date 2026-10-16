# Migrating to id_effect 0.9

This guide is the "if you used X, now do Y" companion to the `0.9.0` section of
[CHANGELOG.md](../CHANGELOG.md). It assumes you are on 0.8. If you are on 0.7, apply
[MIGRATION-0.8.md](MIGRATION-0.8.md) first.

Every intentional public-API break in 0.9 is listed here. A break reported by
`cargo semver-checks` (the `semver` CI job, baseline 0.8.0) that is not listed below is a bug.
The deprecation rule and semver policy are in [STABILITY.md](STABILITY.md).

Each 0.9 change appends its entry to the matching section below.

## Web: validation errors

- `id_effect_axum::json::decode_json_schema`, `JsonSchemaError` and `SchemaJsonErrorBody` are
  deprecated and will be removed in 0.10. They still behave as in 0.8 (first issue only,
  `{path, message}` body). Replace them with `json::decode_json(&schema, &bytes)`, which returns
  `Result<A, Problem>` listing every issue, or with the `route::ValidJson<T>` extractor.
- Clients of those endpoints should expect `Content-Type: application/problem+json` (RFC 9457) and
  read `errors[].pointer` (a JSON Pointer such as `/items/0/name`) instead of `path`
  (`items.0.name`). Each issue also names where it came from in `errors[].in` (`path`, `query`,
  `header`, `body`).

## Data: TLS settings on the pool configs

- `PgPoolConfig` and `MySqlPoolConfig` gain the public fields `tls_mode` and `root_cert`. Code that
  builds them with a struct literal must add them (or `..Default::default()` where available);
  prefer the constructors (`from_url`) and builders (`with_tls_mode`, `with_root_cert_file`).
- Behaviour: with TLS now compiled in, a server that offers TLS is used over (unverified) TLS under
  the default `Prefer` / `Preferred` instead of plaintext. Set `with_tls_mode(PgTlsMode::VerifyFull)`
  / `MySqlTlsMode::VerifyIdentity` and a root certificate for production.

## Data: SQL values and rows

`id_effect_sql` parameters and column values are now typed `SqlValue`s and rows decode by column
(book: "Typed SQL values and rows"). Statements with no parameters (`client.query(sql, &[])`,
`query(sql, vec![])`), transactions, `SqlRow::default()` and `rows.len()` compile unchanged.

- Statements take `&[SqlValue]` / `Vec<SqlValue>` instead of `SqlParam`s:
  `&[SqlParam::Int(1)]` becomes `&[SqlValue::I64(1)]`, `&[1i64.into()]` or `&params![id]`
  (`params!` encodes each argument by reference with `ToSqlValue`; `None` becomes `SqlValue::Null`).
- `row.cells[i]` (now private) becomes `row.get::<T>(i)` or `row.get::<T>("name")`, which returns
  `Result<T, SqlError>` naming the column (`ColumnNotFound`, `UnexpectedNull`, `TypeMismatch`,
  `ValueOutOfRange`, `Decode`) instead of a string to parse. Transitional: `row.text_cells()`
  returns the 0.8 strings.
- `impl SqlClient for X` / `impl SqlTransaction for X`: `query` and `execute` take
  `params: &[SqlValue]`.
- `SqlError` is `#[non_exhaustive]` and has new variants: add a `_ =>` arm to every `match` on it.
- `SqlRow` and parameter vectors are no longer `Eq` / `Hash` (`SqlValue` holds floats); bound on
  `PartialEq` instead.
- `TestSqlClient::recorded_queries()` / `recorded_executes()` return
  `Vec<(String, Vec<SqlValue>)>`; compare the recorded parameters with `SqlValue`s.
- Deprecated, still compiling with a warning, removed in 0.10:
  - `SqlParam` (convert with `SqlValue::from(param)`; `Int` is `I64`, the other variants keep
    their names; `SqlValue == SqlParam` comparisons still compile),
  - `SqlRow::new(cells)`, replaced by `SqlRow::from_pairs([("id", 1i64)])` (named, typed
    columns),
  - `SqlRow::text_cells()`, replaced by `row.get::<T>(idx)`.
