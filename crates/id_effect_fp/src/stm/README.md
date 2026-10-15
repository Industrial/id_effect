# `stm` — Stratum 12: software transactional memory

**Optimistic transactions** over [`TRef`](mod.rs) cells: [`Stm<A, E>`](mod.rs), [`Outcome`](mod.rs), [`commit`](mod.rs) / [`atomically`](mod.rs), plus [`TQueue`](mod.rs), [`TMap`](mod.rs), and [`TSemaphore`](mod.rs). Commits are **serialized** (global lock); a transaction restarts on conflict, and on `Outcome::Retry` its fiber **parks** on the wait lists of the `TRef`s it touched until a commit writes one of them (Effect.ts `STM.retry`). Interrupting a parked transaction deregisters it.

## What lives here

| Item | Role |
|------|------|
| `Stm` | Transactional program: `map`, `flat_map`, `or_else`, `retry`, `check`. |
| `TRef<A>` | Transactional mutable cell — `read_stm`, `write_stm`, `update_stm`, `modify_stm`. |
| `TQueue`, `TMap`, `TSemaphore` | Composite structures built on `TRef`. |
| `commit` / `atomically` | Lift `Stm` to a next-kernel `Effect` (validation, restart on conflict, waker-based retry). |

## What it is used for

- **Composing** atomic read/modify/write sequences without manual lock ordering across many refs.
- **Parking** retry (`Outcome::Retry`) for “wait until queue non-empty” patterns (no spinning, no blocked thread).
- **Layer internals** and advanced coordination where `Mutex` granularity is awkward.

## Best practices

1. **Know the global commit lock** — STM here is correctness-first, not high-contention throughput; profile hot paths.
2. **Keep transactions short** — no I/O inside `Stm` bodies; lift to `Effect` after `commit`.
3. **Avoid** allocating new `TRef` inside retried transactions unless intended — hoists belong outside `commit` when docs warn about it.
4. **Prefer** `coordination` primitives for simple single-channel cases; use STM when multiple refs must move together.

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 12.
- [`kernel`](../kernel/README.md) — `commit` produces `Effect`.
- [`layer`](../layer/README.md) — `LayerGraph` may use `TRef` for planner state (see crate docs).
