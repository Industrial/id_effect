//! Fiber coordination primitives: `Deferred` (one-shot), `Semaphore`, `Latch`, `Queue`,
//! `Channel`, `PubSub`, `Ref`, `SynchronizedRef`, `SubscriptionRef` and `Supervisor`.
//! Filled by `c-coordination`, ported from the v1 root `coordination` module (and
//! `concurrency::supervisor`) with Effect.ts semantics.
//!
//! - **Plain constructors.** `Ref::new`, `Semaphore::new`, `Queue::bounded`, … build the
//!   primitive directly (not `make() -> Effect`); the operations are lazy effects, generic over
//!   their error type and environment where they cannot fail.
//! - **Real suspensions.** A waiting operation (acquire, take, offer, wait) parks the fiber on a
//!   waker in the primitive's FIFO wait list; no thread is blocked or spun, and nothing here uses
//!   a tokio watch channel or a thread-parking run edge (`G-nb` is clean on this directory).
//! - **Interruption.** Interrupting a waiting fiber drops its suspended step, which removes the
//!   waiter; a permit or item handed to it but not yet observed goes back to the primitive.

#[allow(dead_code)] // helpers for the primitives landing in the next commits of this leaf
mod waiters;

mod deferred;
mod latch;
mod ref_;
mod semaphore;
mod supervisor;
mod synchronized_ref;

pub use deferred::*;
pub use latch::*;
pub use ref_::*;
pub use semaphore::*;
pub use supervisor::*;
pub use synchronized_ref::*;
