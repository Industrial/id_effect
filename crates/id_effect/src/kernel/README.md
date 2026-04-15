# `kernel` — Stratum 2: `Effect` and supporting kernel

The **core** of the interpreter model: [`Effect<A, E, R>`](effect.rs) as a lazy computation over environment `R`, plus [`Thunk`](thunk.rs), [`Result`](result.rs) helpers, and [`Reader`](reader.rs)-style environment threading.

## What lives here

| Module | Role |
|--------|------|
| `effect` | `Effect`, `BoxFuture`, `IntoBind`, `succeed`/`fail`/`pure`, `from_async`, `scoped`, `acquire_release`, etc. |
| `thunk` | Lazy suspension / one-shot deferred values used in the implementation. |
| `result` | Result-oriented helpers aligned with bifunctor / monad structure. |
| `reader` | Environment-parameterized operations (implicit `R` in `run`). |

## What it is used for

- **Describing** all side-effecting work as values (`Effect`) composed with `flat_map`, `map`, `effect!`, etc.
- **Bridging** `async` Rust: wrap third-party futures with `from_async` at the edge, not scattered `async fn` in library code (see project skill `.cursor/skills/effect.rs-fundamentals/SKILL.md`).
- **Running** only at boundaries via [`runtime::run_blocking` / `run_async`](../runtime/README.md).

## Best practices

1. **Model library code as `Effect<A, E, R>`** with explicit `<A, E, R>` on composable builders; avoid new `async fn` application functions outside the runtime boundary.
2. **Use `fail(…).into()`** for domain errors so `E: From<DomainError>` stays the single conversion story.
3. **Prefer `effect!`** for do-notation (`~` bind) over manual `flat_map` when readability matters.
4. **Document variance**: `A`/`E` covariant, `R` contravariant — environment is “inputs” to the interpreter.

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 2.
- [`algebra`](../algebra/README.md) — functor/monad laws referenced by `Effect`.
- [`context`](../context/README.md) — type of `R` (tagged services).
- [`runtime`](../runtime/README.md) — executing `Effect`.
