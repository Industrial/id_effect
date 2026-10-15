//! Managed resources on top of [`Scope`](crate::Scope): [`Cache`], [`Pool`] and
//! [`KeyedPool`] (Effect.ts `Cache`, `Pool`, `KeyedPool`).
//!
//! Constructors are plain functions (`Pool::new`, `KeyedPool::new`, `Cache::new`), not effects:
//! building one acquires nothing. A pooled item is checked out into a caller's scope and checked
//! back in by a scope finalizer, so it returns to the pool however the scope closes, interrupt
//! included. Time-to-live is measured on the running fiber's executor clock, so a
//! `TestExecutor`'s virtual time drives expiry. `Scope` itself belongs to the kernel
//! (`scope`).
//!
//! Waiting (for pool capacity, or for a cache lookup in flight) uses this module's own
//! cancel-safe primitives (`gate`), not `coordination`.

mod cache;
mod gate;
mod pool;

pub use cache::*;
pub use pool::*;
