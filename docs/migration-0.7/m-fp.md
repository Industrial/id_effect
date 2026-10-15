# `id_effect_fp`: the 0.7 kernel and waker-based STM retry

0.7 moves `id_effect_fp` to the new kernel. The pure parts (`algebra`
traits, `collections`, `foundation`, `result`, `reader`, `thunk`) are unchanged. The items that
produce or take an `Effect` now use `Effect`:

| item | 0.6 | 0.7 |
|---|---|---|
| `stm::commit`, `stm::atomically` | v1 `Effect<A, E, R>` (a synchronous body, `R: 'static`) | `Effect<A, E, R>` (an async step, `R: Send + Sync`) |
| `traverse_vec`, `sequence_vec`, `traverse_option` | v1 `Effect`, `F: FnMut(A) -> Effect<..> + 'static` | `Effect`; `A, B, E: Send`, `R: Send + Sync`, `F: Send` |
| `select_effect`, `branch_effect` | v1 `Effect` | `Effect`; values and handler closures `Send` |
| `FreeAp<A, E, R>` | over v1 `Effect` | over `Effect`; `Ap2` closure is `Send` |
| `bifoldable::bitraverse::bitraverse` | v1 `Effect` | `Effect` |

```rust
// 0.6
use id_effect::run_blocking;
let cell = run_blocking(commit(TRef::make(0_i32)), ()).expect("cell");
run_blocking(commit(cell.update_stm::<(), _>(|n| n + 1)), ()).expect("update");
assert_eq!(run_blocking(commit(cell.read_stm::<()>()), ()), Ok(1));

// 0.7
use id_effect::Exit;
let cell = commit(TRef::make(0_i32)).block_on().into_result().expect("cell");
commit(cell.update_stm::<(), _>(|n| n + 1)).block_on().into_result().expect("update");
assert_eq!(commit(cell.read_stm::<()>()).block_on(), Exit::succeed(1));
```

- **`R = ()` becomes `R = HNil`.** `commit::<_, _, ()>(..)` turbofishes go away (the default
  environment is `HNil`); run with `.block_on()` (an `Exit`) or `Runtime::run(..).await`.
- **Retry parks instead of spinning (Effect.ts `STM.retry`).** In 0.6 a retrying transaction
  looped on `std::thread::yield_now`, burning a thread. Now `commit` suspends the fiber and
  registers a waker on every `TRef` the attempt read or wrote; the next commit that writes one
  of them wakes it, and the attempt re-runs. A retry on a single-threaded executor (for example
  `TestExecutor`) therefore makes progress instead of hanging.
- **Interruption.** A parked transaction is interruptible; interrupting it removes its waker
  from every cell. A transaction that retries without touching any `TRef` (a bare
  `Stm::retry()`) waits until it is interrupted.
- **Conflicts restart at once** (no yield): a failed commit validation means another
  transaction committed in between.
- **`Stm::or_else` rolls back the retrying branch's writes** before running the alternative, so
  the alternative sees the values from before the first branch. In 0.6 those writes leaked into
  the alternative and could be committed with it. The reads of both branches stay in the retry
  wait set.
- **A typed failure from an inconsistent snapshot restarts the attempt.** `Stm::fail` ends the
  commit only when every cell the attempt read is still at the version it saw; otherwise the
  attempt re-runs on fresh values.
- **An unmigrated (v1) caller** wraps the committed effect with the bridge:
  `use id_effect::compat::IntoV1; commit(stm).into_v1::<R>()`.
