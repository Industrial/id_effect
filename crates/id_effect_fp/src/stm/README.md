# `stm` — Stratum 12: software transactional memory

**Optimistic transactions** over [`TRef`](mod.rs) cells: [`Stm<A, E>`](mod.rs), [`Outcome`](mod.rs), [`commit`](mod.rs) / [`atomically`](mod.rs), plus [`TQueue`](mod.rs), [`TMap`](mod.rs), and [`TSemaphore`](mod.rs). Commits are **serialized** (global lock); transactions retry on conflict or `Outcome::Retry`.

## What lives here

| Item | Role |
|------|------|
| `Stm` | Transactional program: `map`, `flat_map`, `or_else`, `retry`, `check`. |
| `TRef<A>` | Transactional mutable cell — `read_stm`, `write_stm`, `update_stm`, `modify_stm`. |
| `TQueue`, `TMap`, `TSemaphore` | Composite structures built on `TRef`. |
| `commit` / `atomically` | Lift `Stm` to `Effect` (blocking retry loop + validation). |

## What it is used for

- **Composing** atomic read/modify/write sequences without manual lock ordering across many refs.
- **Blocking** retry (`Outcome::Retry`) for “wait until queue non-empty” patterns.
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
