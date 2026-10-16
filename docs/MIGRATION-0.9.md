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

## Data: SQL values and rows

Filled in by the `SqlValue` / `SqlRow` change (release spec D2).
