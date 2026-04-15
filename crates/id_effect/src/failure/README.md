# `failure` — Stratum 4: structured failure

Algebraic **failure** and **terminal outcomes**: [`Cause<E>`](cause.rs) (errors with semigroup-style combination), [`Exit<A, E>`](exit.rs) (fiber-level success/failure/interrupt), and [`Or<L, R>`](union.rs) for error unions.

## What lives here

| Module | Role |
|--------|------|
| `cause` | `Cause<E>` — structured failure (e.g. defects, interruptions, failures). |
| `exit` | `Exit<A, E>` — how a fiber completes (`Success`, `Failure`, `Interrupt`, …). |
| `union` | `Or<L, R>` — coproduct-style error tagging. |

## What it is used for

- **Uniform handling** of defects vs failures in concurrent and streaming code.
- **Composing** error contexts where `Cause`’s `Semigroup` instance applies.
- **Expressing** fiber completion without collapsing everything to `Result` when you need interruption semantics.

## Best practices

1. **Map into your `E`** at service boundaries with `From` / `Into` rather than ad hoc string errors.
2. **Prefer `fail(…).into()`** in `Effect` for domain errors (see project Effect.rs skill).
3. **Do not** use `Exit` where a plain `Result` suffices — reserve for fiber/runtime-level completion stories.
4. **Test both** success and error branches when combinators touch `Cause` or `Exit` (`TESTING.md`).

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 4.
- [`concurrency`](../concurrency/README.md) — `Exit` ties to fibers.
- [`kernel`](../kernel/README.md) — `Effect` error channel `E`.
