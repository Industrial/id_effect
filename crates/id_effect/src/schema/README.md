# `schema` — data, equality, ordering and parsing

| Module | Items |
|--------|-------|
| `parse` | `Schema` (decode / encode from `Unknown`), `ParseError`; combinators such as `struct_`, `tuple`, `optional`, `refine`. |
| `parse_errors` | `ParseErrors`: accumulated errors with paths. |
| `has_schema` | `HasSchema` (+ `derive(Schema)`). |
| `brand` | `Brand` / `RefinedBrand`: newtypes for domain tagging. |
| `data` / `equal` / `order` | `EffectData` (`DataStruct`, `DataTuple`, `DataError`), `Equal` / `EffectHash`, `Ordering` / `DynOrder`. |
| `redacted` | `Redacted<T>`: values that never print. |
| `serde_bridge` / `json_schema_export` | Feature `schema-serde`: `serde_json::Value` ↔ `Unknown`, JSON Schema export. |

Treat `Unknown` as hostile until a `Schema` decodes it; report `ParseError`s with their path,
not raw secrets. Keep `Equal` / `EffectHash` consistent with `Eq` / `Hash`. See
[`SPEC.md`](SPEC.md) for the schema notation and edge cases.
