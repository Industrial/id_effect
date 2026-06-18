---
name: id_effect-schema
description: >-
  Teaches id_effect schema: Unknown at boundaries, struct_/array/optional/union
  combinators, validation refinements, and ParseError handling. Use when parsing
  JSON/API payloads, defining DTOs, or validating external input — analogous to
  Effect.ts Schema at boundaries.
---

# id_effect Schema

**Part IV ch14**. Default: **schema at boundaries**, typed domain inside.

**Prerequisite**: `id_effect-fundamentals`.

## Decision tree

```
External unknown input (HTTP, file, env, DB row)?
  → Unknown + Schema → Effect (never silent null)

Domain type already validated?
  → plain Rust struct/enum — no re-parse

Expected failure in pipeline?
  → typed E / fail(ParseError::…) — see id_effect-errors
```

## Primitives & structs

```rust
use id_effect::schema::{string, i64, struct_};

let user_schema = struct_!(User {
    name: string(),
    age:  i64(),
});
```

## Composition

```rust
optional(u16())           // Option<T>
array(string())           // Vec<T>
union_![ … ]              // tagged variants
object([("k", schema)])   // ad-hoc objects
```

## Parsing

Parse **`Unknown`** at the boundary; map `ParseError` to domain errors before business logic:

```rust
schema.parse(unknown).map_error(AppError::InvalidInput)
```

Errors include field paths: `"[2].email: expected string, got null"`.

## Not this → but that

| Not this | But that |
|----------|----------|
| `serde_json::from_str` + unwrap in domain | schema parse inside boundary module |
| Re-parsing in every handler | parse once; pass typed values inward |
| `String` for IDs/emails without validation | schema + refinement/branded types |
| Manual match on loose `Value` | `struct_!` / `union_!` |

## Next

- HTTP boundaries: [id_effect-integration](../id_effect-integration/SKILL.md)
