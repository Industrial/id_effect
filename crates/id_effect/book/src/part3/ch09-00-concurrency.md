# Concurrency & Fibers — Structured Async

Async Rust gives you the ability to do many things concurrently. The challenge is doing it *safely* — without fire-and-forget tasks that outlive their parent, without silent failures when a task panics, and without resource leaks when tasks are cancelled.

id_effect uses **fibers** for structured concurrency. A fiber is a lightweight, interruptible task with a typed result, an explicit lifecycle and guaranteed cleanup. Fibers run on an [`Executor`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/runtime/executor.rs): `TokioExecutor` in production (each fiber is an ordinary tokio task; `Effect::block_on` and `Runtime::global()` own one for you) and the deterministic `TestExecutor` in tests. Parallel combinators can additionally be throttled by the [**Compute Fabric**](./ch12-00-compute-fabric.md) (`Concurrency::Inherit`).

This chapter covers forking and joining fibers, the parallel combinators (`zip_par`, `all`, `race`, `timeout`), interruption, `FiberRef` for fiber-local state, and **supervision** (`supervised`, `Supervisor`).
