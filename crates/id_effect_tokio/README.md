# id_effect_tokio

Tokio runtime adapter for [`id_effect`](https://github.com/Industrial/id_effect), on the 0.7
kernel (`id_effect::next`).

- The executor lives in core: `TokioExecutor` (re-exported here) runs every fiber as an ordinary
  tokio task, with real wakers and tokio timers. Fibers are `Send`; a sleeping fiber parks on the
  tokio timer instead of spinning a blocking-pool thread as in 0.6.
- `TokioRuntime` pairs the `next` `Runtime` with the tokio runtime it runs on, owned
  (`new_current_thread`, `new_multi_thread`) or ambient (`current`, `from_handle`). It offers
  `run` (async edge), `fork` (daemon root fiber), `spawn` (a tokio task with a `JoinHandle`),
  `now`, and `block_on` as a program edge on an owned runtime.
- Sleep and yield are the kernel's `id_effect::next::{sleep, yield_fiber}`.

Tower, Axum and other Tokio-based adapters depend on this crate for that wiring.

## Quick start

```rust
use std::time::Duration;
use id_effect::Exit;
use id_effect::next::{HNil, sleep, succeed};
use id_effect_tokio::TokioRuntime;

let rt = TokioRuntime::new_current_thread().expect("tokio runtime");
rt.block_on(async {
  assert!(matches!(rt.run(sleep::<(), HNil>(Duration::from_millis(1))).await, Exit::Success(())));
  let fiber = rt.fork(succeed::<u8, (), HNil>(42));
  assert!(matches!(fiber.join_future().await, Exit::Success(42)));
});
```

## Examples

| Example | Shows |
|---------|-------|
| `106_tokio_runtime` | `TokioRuntime::run` with sleep / yield on the Tokio driver |
| `107_tokio_fork_contract` | `TokioRuntime::fork`, join, and interruption |
| `108_tokio_clock` | monotonic `now`, async `sleep`, `yield_fiber` on an existing runtime |
| `109_tokio_end_to_end` | typed capabilities, an async step, `catch`, concurrent fibers |

Run one with `cargo run -p id_effect_tokio --example 106_tokio_runtime`.
