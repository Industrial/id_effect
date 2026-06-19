# Parser Combinators

`id_effect_parse` brings small, composable parsers to the workspace — the same functional pattern as schema combinators in Part IV, but oriented toward **text and byte streams** rather than JSON `Unknown` values.

## The `Parser` type

A [`Parser<I, O, E>`](https://docs.rs/id_effect_parse/latest/id_effect_parse/struct.Parser.html) wraps a function `I -> Result<(O, I), E>`: parsed output plus **remaining input**.

```rust
use id_effect_parse::{char, int, parse_str, Parser};

let number = char('(')
    .and_then(|_| int())
    .and_then(|n| char(')').map(move |_| n));

let (value, rest) = parse_str(&number, "(42) rest").unwrap();
assert_eq!(value, 42);
assert_eq!(rest, " rest");
```

Core combinators:

| Combinator | Role |
|------------|------|
| `map` | transform parsed output |
| `and_then` | sequence dependent parsers |
| `alt` | try another parser on failure |
| `many` | repeat until the inner parser fails |

Built-ins such as `char`, `tag`, `int`, and `ws` cover common text needs.

## Pretty printing

The [`Pretty`](https://docs.rs/id_effect_parse/latest/id_effect_parse/trait.Pretty.html) trait builds [`Doc`](https://docs.rs/id_effect_parse/latest/id_effect_parse/enum.Doc.html) values — a Wadler-style document tree rendered with a line width budget:

```rust
use id_effect_parse::{Doc, Pretty};

let doc = Doc::text("users")
    .cat(Doc::line())
    .cat(["alice", "bob"].pretty())
    .group();

println!("{}", doc.render(40));
```

Use pretty printers for debug output, REPLs, and human-readable config — not for wire formats (use `Codec` instead).

## Invertible codecs

[`Codec`](https://docs.rs/id_effect_parse/latest/id_effect_parse/struct.Codec.html) pairs parse and print so formats round-trip:

```rust
use id_effect_parse::codec::quoted_string;

let codec = quoted_string();
let wire = codec.print(&"hello".to_string());
let (parsed, _) = codec.parse(wire).unwrap();
assert_eq!(parsed, "hello");
```

When you need lossless serialization with a parser-shaped API, start with `Codec::new`.

## Diffs

[`Diff<T>`](https://docs.rs/id_effect_parse/latest/id_effect_parse/enum.Diff.html) describes value-level changes (`Unchanged`, `Added`, `Removed`, `Changed`). Helpers like `diff_values` and `diff_option` support config drift and snapshot tests.

## Parsing `Stream` chunks

Collect stream chunks, flatten, then parse — [`parse_stream`](https://docs.rs/id_effect_parse/latest/id_effect_parse/fn.parse_stream.html) for typed buffers, [`parse_text_stream`](https://docs.rs/id_effect_parse/latest/id_effect_parse/fn.parse_text_stream.html) for UTF-8 text:

```rust
use id_effect::{Chunk, Stream, run_blocking};
use id_effect_parse::{parse_text_stream, tag};

let parser = tag("ping");
let stream = Stream::from_iterable(vec![Chunk::from_vec(b"ping".to_vec())]);
let value = run_blocking(parse_text_stream(&parser, stream), ()).unwrap();
assert_eq!(value, "ping");
```

## Schema bridge (stub)

Boundary validation for external data still belongs to **`id_effect::schema`** (Part IV). `SchemaBridgeStub` reserves a future path from `Schema` values to parsers; until then, keep schema at the edge and parsers for internal/text protocols.

## Next steps

- Part IV [`Schema`](../part4/ch14-00-schema.md) for JSON and API boundaries
- Part IV [`Streams`](../part4/ch13-00-streams.md) for chunk backpressure and collection
- Workspace crate: `crates/id_effect_parse/`
