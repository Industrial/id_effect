# Concurrency & Fibers — Structured Async

Async Rust gives you the ability to do many things concurrently. The challenge is doing it *safely* — without fire-and-forget tasks that outlive their parent, without silent failures when a task panics, and without resource leaks when tasks are cancelled.

id_effect uses **Fibers** for structured concurrency. A Fiber is a lightweight, interruptible async task with a typed result, an explicit lifecycle, and guaranteed cleanup.

Fibers spawn through [**Compute Fabric**](./ch12-00-compute-fabric.md): [`ThreadSleepRuntime`](../../src/runtime/rt.rs) admits work on a shared [`FiberPool`](../../src/compute/fiber_pool.rs) instead of one OS thread per fiber, and [`TokioRuntime`](../../id_effect_tokio/src/lib.rs) gates blocking-pool tasks with the same admission controller.

This chapter covers spawning fibers, joining them, cancelling them gracefully, using `FiberRef` for fiber-local state, and **supervision** (restart policies tied to [`Scope`](./ch10-02-scopes-finalizers.md) and [`CancellationToken`](./ch09-03-cancellation.md)).
