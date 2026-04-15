# Schema — Parse, Don't Validate

Data enters your program from the outside world: HTTP request bodies, database rows, configuration files, message queue payloads. All of it is untrusted. All of it needs to be checked.

The naive approach is to deserialise first and validate later — accept a `User` struct via serde, then check that `email` is non-empty and `age` is positive in a separate step. The problem: your type says `User` but your program has a `User` that might have an empty email. The type lies.

The better approach is *parse, don't validate*: transform untrusted input into trusted types in one step. If the parse succeeds, you have a valid `User`. If it fails, you have a structured `ParseError` that tells you exactly what was wrong.

id_effect's `schema` module is built on this principle.

## What This Chapter Covers

- **`Unknown`** — the type for unvalidated wire data ([next section](./ch14-01-unknown.md))
- **Schema combinators** — the building blocks for describing data shapes ([ch14-02](./ch14-02-combinators.md))
- **Validation and refinement** — `refine`, `filter`, and `Brand` for domain constraints ([ch14-03](./ch14-03-validation.md))
- **`ParseErrors`** — structured, accumulating error reports ([ch14-04](./ch14-04-parse-errors.md))
