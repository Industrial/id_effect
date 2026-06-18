# `id_effect_parse`

Parser combinator toolkit for the [`id_effect`](https://docs.rs/id_effect) workspace:

- **`Parser<I, O, E>`** — `map`, `and_then`, `alt`, `many`, and `parse`
- **`Doc` / `Pretty`** — Wadler-style pretty documents
- **`Codec`** — invertible parse + print pairs
- **`Diff`** — value-level change descriptions
- **`parse_stream` / `parse_text_stream`** — collect [`Stream`](https://docs.rs/id_effect/latest/id_effect/struct.Stream.html) chunks then parse
- **`SchemaBridgeStub`** — placeholder for future `Schema` integration

See mdBook Part V chapter 20 (`crates/id_effect/book/src/part5/ch20-00-parser-combinators.md`).

## Quick start

```rust
use id_effect_parse::{char, int, parse_str, Parser};

let pair = char('(')
    .and_then(|_| int())
    .and_then(|n| char(')').map(move |_| n));

let (value, rest) = parse_str(&pair, "(42)rest").unwrap();
assert_eq!(value, 42);
assert_eq!(rest, "rest");
```

## Verify

```bash
cargo test -p id_effect_parse
cargo clippy -p id_effect_parse -- -D warnings
```
