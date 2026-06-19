---
name: id_effect-parse
description: >-
  Parser combinators, Pretty documents, invertible Codec, Diff, and Stream parse
  bridges in id_effect_parse. Use when parsing text/byte protocols, pretty-printing
  debug output, or round-tripping wire formats — not for JSON/API boundaries (use
  id_effect-schema).
---

# id_effect Parse

**Part V ch20**. Text and byte protocols inside the effect graph; schema stays at boundaries.

**Prerequisite**: `id_effect-fundamentals`, `id_effect-streams` for `parse_stream`.

## Decision tree

```
External JSON/API payload?
  → id_effect-schema (Unknown + Schema)

Text or byte protocol (CLI, logs, custom wire)?
  → id_effect_parse Parser / Codec

Need human-readable debug output?
  → Pretty / Doc

Compare config snapshots?
  → Diff
```

## Parser combinators

```rust
use id_effect_parse::{char, int, parse_str, Parser};

let pair = char('(')
    .and_then(|_| int())
    .and_then(|n| char(')').map(move |_| n));

let (value, rest) = parse_str(&pair, "(42)").unwrap();
```

| Combinator | Use |
|------------|-----|
| `map` | transform output |
| `and_then` | dependent sequencing |
| `alt` | fallback parser |
| `many` | zero-or-more repeat |

Built-ins: `char`, `tag`, `int`, `signed_int`, `bool_lit`, `float`, `ws`, `optional`, `sep_by`, `between`.

## Schema bridge

```rust
use id_effect::SchemaParser;
use id_effect_parse::SchemaBridge;

#[derive(SchemaParser)]
struct User { name: String, age: i64 }

let user = User::parser().parse(r#"{"name":"Ada","age":36}"#.into()).unwrap().0;
let via_bridge = SchemaBridge::parser_for_json(User::schema());
```

## Codec (invertible)

```rust
use id_effect_parse::codec::quoted_string;

let codec = quoted_string();
let wire = codec.print(&value);
let (parsed, _) = codec.parse(wire).unwrap();
```

## Stream bridge

```rust
use id_effect::{Chunk, Stream, run_blocking};
use id_effect_parse::{parse_text_stream, tag};

let stream = Stream::from_iterable(vec![Chunk::from_vec(bytes)]);
run_blocking(parse_text_stream(&parser, stream), env)?;
```

## Verify

```bash
cargo test -p id_effect_parse
cargo clippy -p id_effect_parse -- -D warnings
```

## See also

- `id_effect-schema` — Part IV boundary parsing
- Book: `crates/id_effect/book/src/part5/ch20-00-parser-combinators.md`
- Crate: `crates/id_effect_parse/`
