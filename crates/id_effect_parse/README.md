# `id_effect_parse`

Parser combinator toolkit for the [`id_effect`](https://docs.rs/id_effect) workspace:

- **`Parser<I, O, E>`** — `map`, `and_then`, `alt`, `many`, `optional`, `sep_by`, `between`
- **`byte`** — byte-buffer parsers (`byte_tag`, `byte_int`, …)
- **`json`** — JSON text → [`Unknown`](https://docs.rs/id_effect/latest/id_effect/schema/enum.Unknown.html)
- **`Doc` / `Pretty`** — Wadler-style pretty documents with flat/broken layout
- **`Codec`** — invertible parse + print pairs (`int_codec`, `list`, `pair`, …)
- **`Diff`** — value-level change descriptions
- **`SchemaBridge`** — [`Schema`](https://docs.rs/id_effect/latest/id_effect/schema/struct.Schema.html) ↔ text parsers
- **`#[derive(SchemaParser)]`** — generates `schema()`, `parser()`, and [`HasSchema`](https://docs.rs/id_effect/latest/id_effect/schema/trait.HasSchema.html)
- **`parse_stream` / `parse_text_stream`** — collect [`Stream`](https://docs.rs/id_effect/latest/id_effect/struct.Stream.html) chunks then parse

See mdBook Part V chapter 20 (`crates/id_effect/book/src/part5/ch20-00-parser-combinators.md`).

## Quick start

```rust
use id_effect::SchemaParser;
use id_effect_parse::{char, int, parse_str, Parser, SchemaBridge};

#[derive(SchemaParser)]
struct User {
  name: String,
  age: i64,
}

let pair = char('(')
    .and_then(|_| int())
    .and_then(|n| char(')').map(move |_| n));

let (value, rest) = parse_str(&pair, "(42)rest").unwrap();
assert_eq!(value, 42);
assert_eq!(rest, "rest");

let user = User::parser()
    .parse(r#"{"name":"Ada","age":36}"#.to_string())
    .unwrap()
    .0;
assert_eq!(user.name, "Ada");
```

## Example

```bash
cargo run -p id_effect_parse --example 010_schema_parser
```

## Verify

```bash
cargo test -p id_effect_parse
cargo clippy -p id_effect_parse -- -D warnings
```
