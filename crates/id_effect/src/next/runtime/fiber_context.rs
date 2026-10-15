//! `FiberContext`: per-fiber state (id, interrupt flag, mask, waker, parent and children, fiber
//! refs). Filled by `k-core-runloop`, then extended by `k-fiber-tree`, `k-scope-fiberref` and
//! `k-runloop-opt`.
