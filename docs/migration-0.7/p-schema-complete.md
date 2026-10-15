# Schema: tuple / struct codecs up to arity 12, `derive(Schema)` field-shape fixes

Additive only — no 0.6 code needs to change.

```rust
use id_effect::schema::{i64, string, bool_, struct5};

// 0.6: tuple/tuple3/tuple4 and struct_/struct3/struct4 were the widest combinators, so wider
// shapes had to nest tuples.
// 0.7: `tuple5`..`tuple12` and `struct5`..`struct12` follow the same contract as the 4-ary ones.
let user = struct5(
  "id", i64::<()>(), "name", string(), "admin", bool_(), "age", i64(), "nick", string(),
);
```

- **Wire shape** is unchanged: `tupleN` decodes `Unknown::Array` of length `N` and prefixes
  errors with the position; `structN` decodes `Unknown::Object`, prefixes errors with the field
  name, and reports a missing key as `missing field <name>`.
- **`#[derive(Schema)]`** already accepted any number of fields; these field shapes used to
  fail to compile and now work:
  - `i32` fields (the generated encode closure dereferenced a value);
  - `Option<bool>`, `Option<f64>`, `Option<i32>`, `Vec<i32>`;
  - `Vec<T>` where `T: HasSchema` (the generated code used `<T as HasSchema<I = Unknown>>`,
    which Rust rejects, E0229).
