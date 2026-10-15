# `failure` — causes and exits

Kernel-independent failure types, re-exported at the crate root:

| Module | Items |
|--------|-------|
| `cause` | `Cause<E>`: `Fail(E)`, `Die(Defect)`, `Interrupt(FiberId)`, `Both` (parallel) and `Then` (sequential) composites. |
| `exit` | `Exit<A, E>`: `Success(A)` or `Failure(Cause<E>)`, what `block_on` / `join` return. |
| `union` | `Or<L, R>`: the error union spelled by `err!(A \| B)`. |
| `fiber_id` / `never` / `pretty` | `FiberId`, the uninhabited `Never` (the default `E`), cause rendering. |

A typed failure (`fail(e)`) is a `Cause::Fail`; a panic inside an effect becomes `Cause::Die`;
interruption is `Cause::Interrupt`. Map into your domain `E` at boundaries with `map_err` /
`From` rather than strings, and keep `Exit` for run edges and fiber joins (`into_result()`
when a `Result` is enough).
