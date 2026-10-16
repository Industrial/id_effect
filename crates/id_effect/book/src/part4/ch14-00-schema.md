# Schema — Parse, Don't Validate

Data enters your program from the outside world: HTTP request bodies, database rows, configuration files, message queue payloads. All of it is untrusted. All of it needs to be checked.

The naive approach is to deserialise first and validate later — accept a `User` struct via serde, then check that `email` is non-empty and `age` is positive in a separate step. The problem: your type says `User` but your program has a `User` that might have an empty email. The type lies.

The better approach is *parse, don't validate*: transform untrusted input into trusted types in one step. If the parse succeeds, you have a valid `User`. If it fails, you have a structured `ParseError` that tells you exactly what was wrong.

id_effect's `schema` module is built on this principle. A `Schema<A, I, E>` is a bidirectional
codec: it decodes a wire value `I` (or a tree-shaped `Unknown`) into a semantic `A` and encodes
`A` back to `I`. `E` is a phantom tag (any `EffectData` type, `()` is fine) that keeps schemas
of different domains apart. Schemas are synchronous and kernel-independent: `decode` returns a
plain `Result<A, ParseError>`, and you lift it into an `Effect` at the boundary.

Runnable examples: `083_schema_primitive` … `086_parse_error_paths` in
`crates/id_effect/examples/`; every combinator is round-trip tested in
`crates/id_effect/tests/schema_roundtrip.rs`.

## What This Chapter Covers

- **`Unknown`** — the type for unvalidated wire data ([next section](./ch14-01-unknown.md))
- **Schema combinators** — the building blocks for describing data shapes, and JSON Schema export ([ch14-02](./ch14-02-combinators.md))
- **Validation and refinement** — `refine`, `filter`, `transform`, built-in validators (`email`, `uuid`, `url`, `length`, `range`, `pattern`) and `Brand` for domain constraints ([ch14-03](./ch14-03-validation.md))
- **`ParseError` / `ParseErrors`** — path-aware error reports, fail-fast or all-at-once ([ch14-04](./ch14-04-parse-errors.md))
