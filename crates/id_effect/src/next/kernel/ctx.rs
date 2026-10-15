//! `Ctx<R>`: the environment (`Arc<R>`) and the current fiber (`Arc<FiberContext>`) seen by a
//! running effect, with `env()`/`fiber()`. Filled by `k-core-runloop`.
