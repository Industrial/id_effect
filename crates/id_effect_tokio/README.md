# id_effect_tokio

Tokio runtime adapter for [`id_effect`](https://github.com/Industrial/id_effect).

- `TokioRuntime` implements `id_effect::Runtime`: `sleep` and `yield_now` run on the Tokio timer
  driver, and forked effects run on Tokio's blocking pool (the `Effect` interpreter is not `Send`,
  so it is never handed to `tokio::spawn`).
- `spawn_blocking_run_async` drives a non-`Send` effect graph with the real async interpreter on a
  blocking worker — use it for effects that need Tokio I/O or timers.
- Building a `TokioRuntime` keeps any `ComputeFabric` you installed beforehand.

Tower, Axum and other Tokio-based adapters depend on this crate for that wiring.

## Quick start

```rust
use std::time::Duration;
use id_effect::{Runtime, run_async, succeed};
use id_effect_tokio::{TokioRuntime, yield_now};

let rt = TokioRuntime::new_current_thread().expect("tokio runtime");
rt.block_on(async {
  run_async(rt.sleep(Duration::from_millis(1)), ()).await.unwrap();
  run_async(yield_now(&rt), ()).await.unwrap();
  assert_eq!(run_async(succeed::<u8, (), ()>(42), ()).await, Ok(42));
});
```

## Examples

| Example | Shows |
|---------|-------|
| `106_tokio_runtime` | `TokioRuntime` as an `id_effect::Runtime` (sleep / yield) |
| `107_tokio_fork_contract` | `run_fork` on `TokioRuntime`, then `join` |
| `108_tokio_clock` | monotonic `now`, async `sleep`, cooperative `yield_now` |
| `109_tokio_end_to_end` | capability DI, `effect!`, streams and `catch` on Tokio |

Run one with `cargo run -p id_effect_tokio --example 106_tokio_runtime`.
