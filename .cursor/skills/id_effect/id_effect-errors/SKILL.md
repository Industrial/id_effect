---
name: id_effect-errors
description: >-
  Teaches id_effect error handling: Exit, Cause, typed failures vs defects,
  recovery combinators, error accumulation, and CLI exit mapping. Use when handling
  failures, designing error types, retry/catch logic, or mapping process exit codes.
---

# id_effect Errors

**Part III ch8** + CLI exit codes (ch16). Builds on `id_effect-fundamentals`.

## Exit vs Result

| Runner | Returns | Use |
|--------|---------|-----|
| `run_blocking` | `Result<A, E>` | Application logic — typed `E` only |
| `run_test` | `Exit<A, E>` | Tests — full failure taxonomy |
| Fiber `join` | `Exit<A, E>` | Structured concurrency |

```rust
enum Exit<A, E> {
    Success(A),
    Failure(Cause<E>),
}
```

**Cause variants:** `Fail(e)` typed error · `Die(s)` defect/panic · `Interrupt` cancellation.

## Converting Exit

```rust
let result: Result<User, AppError> = exit.into_result(|cause| match cause {
    Cause::Fail(e) => AppError::Expected(e),
    Cause::Die(_) => AppError::Defect,
    Cause::Interrupt(_) => AppError::Cancelled,
});

// When defects should panic:
let result: Result<User, DbError> = exit.into_result_or_panic();
```

## Recovery combinators

```rust
effect.catch(|e| recover(e))
effect.or_else(|e| fallback())
effect.retry(Schedule::exponential(100.ms()).take(3))
effect.map_error(AppError::from)
```

Compose retry **on the Effect description**, not inside running async code.

## Error accumulation

When collecting multiple validation failures, use accumulation patterns from ch8-04 rather than fail-fast on the first field error.

## CLI / process edge

At `main`, map full `Exit` to process codes via **`id_effect_cli::exit_code_for_exit`** (see book ch16-01).

```rust
// run_main + exit_code_for_cause for typed failures at the edge
```

## Not this → but that

| Not this | But that |
|----------|----------|
| `panic!` for expected business failures | `fail(ExpectedError::…)` |
| `.unwrap()` in domain logic | typed `E` + `map_error` / `catch` |
| Ignoring `Cause::Die` in tests | assert with `run_test` + `matches!(exit, …)` |
| Stringly-typed errors everywhere | focused error enums per layer |

## Next

- Concurrency + cancellation: [id_effect-concurrency](../id_effect-concurrency/SKILL.md)
- Testing exits: [id_effect-testing](../id_effect-testing/SKILL.md)
