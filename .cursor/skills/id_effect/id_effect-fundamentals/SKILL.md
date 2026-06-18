---
name: id_effect-fundamentals
description: >-
  Teaches id_effect foundations: Effect as a lazy description, Effect<A,E,R>, effect!
  do-notation, the ~ bind operator, map/flat_map/pipe, IntoBind for Result, and when
  not to use the macro. Use when learning id_effect, writing pure effect programs,
  or before capability DI and integration crates.
---

# id_effect Fundamentals

Foundational patterns from **Part I** of the id_effect book. Read this before `id_effect-capabilities`.

## Mental model

An `Effect<A, E, R>` is a **recipe**, not a cake. Building it runs nothing; execution happens only at the edge via `run_blocking`, `run_async`, or `run_with`.

```rust
let recipe: Effect<i32, String, ()> = succeed(42);
let doubled = recipe.map(|x| x * 2); // still no execution
let result = run_blocking(doubled)?;  // now it runs
```

**Separation of concerns in the type:**

| Param | Role |
|-------|------|
| `A` | Success value |
| `E` | Typed failure |
| `R` | Requirements (capabilities) — see `id_effect-capabilities` |

## Creating effects

```rust
use id_effect::{Effect, succeed, fail, effect};

let ok: Effect<i32, String, ()> = succeed(42);
let err: Effect<i32, String, ()> = fail("oops".into());

fn program() -> Effect<i32, String, ()> {
    effect! {
        let a = ~ succeed(1);
        let b = ~ succeed(2);
        a + b
    }
}
```

Prefer **`fn -> Effect<…>`** returning a description over **`async fn`** that hides effects inside (see `appendix-b-migration.md`).

## The `effect!` macro

- Builds a lazy `Effect` from sequential steps.
- **`~expr`** — bind: run this effect; on failure, short-circuit.
- **`~Key`** inside `|r|` — capability lookup (DI); covered in `id_effect-capabilities`.
- **`effect!` stays sequential** — no parallel binds; use `fiber_all` or stream operators for concurrency.

```rust
effect! {
    ~ log_event("start");
    let user = ~ fetch_user(42);
    user.name
}
```

## The `~` operator rules

| Rule | Detail |
|------|--------|
| Prefix only | `~ step()` — postfix `step() ~` is removed |
| Inside `effect!` only | Outside → compile error |
| Works on full chains | `~ fetch(id).map_error(...).retry(...)` |
| In `if` / loops | Both branches can be effects; loops are sequential |
| Not inside `async move` | Use `.await` inside `from_async` bodies; `~` outside |

## Transforming without running

```rust
effect.map(|x| x * 2)
effect.flat_map(|x| succeed(x + 1))
effect.map_error(AppError::from)
```

## Kernel `IntoBind` (not DI)

`Effect` and `Result` implement `IntoBind` for `~` in `effect!`:

```rust
effect! {
    let n = ~Ok(42);
    let _ = ~ logger.info("");
    n
}
```

Do **not** confuse this with **`~ServiceKey`** capability lookup — services use `~Key` / `require!(Key)`.

## When to use `effect!` vs combinators

| Prefer | When |
|--------|------|
| `effect!` | Multi-step sequencing, locals, branching |
| `.map` / `.flat_map` | Single-step transforms, reusable pipelines |
| Plain Rust + `succeed` | Trivial pure wrappers |

## When **not** to use `effect!`

- One-liner `map` chains with no branching.
- When you need **parallel** steps — use `fiber_all`, `Stream::map_par_n`, or Rayon bulk APIs (`id_effect-streams`).

## Running at the edge

```rust
run_blocking(program)?;
run_blocking(program, env)?;
```

Use **`run_test`** in unit tests — see `id_effect-testing`.

## One macro per function

Keep **`effect!` at the top level** of each function. Extract nested logic into named `fn -> Effect`.

## Next

- Dependencies: [id_effect-capabilities](../id_effect-capabilities/SKILL.md)
- Errors: [id_effect-errors](../id_effect-errors/SKILL.md)
