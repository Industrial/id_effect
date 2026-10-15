# m-tokio: `id_effect_tokio` on the 0.7 kernel

`id_effect_tokio` is now a thin layer over the kernel's `TokioExecutor` and `Runtime`.
Every fiber is an ordinary tokio task with a real waker; the 0.6 driver that ran each forked
effect with `run_blocking` on tokio's blocking pool (a busy-polling OS thread per fiber) is gone.

## What changed

| 0.6 | 0.7 |
|---|---|
| `TokioRuntime: id_effect::Runtime` (v1 trait) | `TokioRuntime` wraps `id_effect::Runtime` (`runtime()`, `AsRef<Runtime>`, `From<TokioRuntime> for Runtime`) |
| `run_fork(&rt, \|\| (eff, env))` → v1 `FiberHandle` | `rt.fork(eff)` → `FiberHandle` (`join_future().await`, `join()`, `interrupt_now()`) |
| `run_async(eff, env).await` | `rt.run(eff).await` → `Exit<A, E>` (provide capabilities first: `eff.provide_all(hlist![..])`) |
| `spawn_blocking_run_async(&handle, f)` / `rt.spawn_blocking_run_async(f)` | removed; `rt.spawn(eff)` → `tokio::task::JoinHandle<Exit<A, E>>` (effects are `Send`) |
| `rt.sleep(d)`, `id_effect_tokio::yield_now(&rt)` | `id_effect::sleep(d)`, `id_effect::yield_fiber()` (they use the fiber's executor) |
| `TokioRuntime::with_fabric`, `rt.fabric()`; constructors install a default `ComputeFabric` | removed; fiber admission moves to `compute` (`c-fabric-scheduler`) |
| — | `TokioExecutor` re-exported; `rt.executor()` |

Unchanged: `TokioRuntime::{current, from_handle, new_current_thread, new_multi_thread, handle,
now, block_on}` (`block_on` still needs an owned runtime and panics otherwise). The owned
runtimes now enable all compiled tokio drivers, and an owned runtime is shut down with
`shutdown_background` when the last clone drops, so dropping one inside an async context no longer
panics.

## Before / after

```rust
// 0.6
use id_effect::{Runtime, kernel::succeed, run_fork};
use id_effect_tokio::TokioRuntime;

let rt = TokioRuntime::new_current_thread()?;
let out = rt.block_on(async {
  run_fork(&rt, || (succeed::<u8, (), ()>(7), ())).join().await
});
assert_eq!(out, Ok(7));
```

```rust
// 0.7
use id_effect::Exit;
use id_effect::{HNil, succeed};
use id_effect_tokio::TokioRuntime;

let rt = TokioRuntime::new_current_thread()?;
let out = rt.block_on(rt.fork(succeed::<u8, (), HNil>(7)).join_future());
assert!(matches!(out, Exit::Success(7)));
```

Benches keep their IDs (`gate/spawn_join_10k`, `gate/sleep_1k_x_1ms`, `gate/sleep_cpu_1k_x_1ms`,
`startup_tokio_first_spawn`) and now measure the 0.7 kernel.
