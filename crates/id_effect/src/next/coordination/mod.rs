//! Fiber coordination primitives: `Deferred` (one-shot), `Semaphore`, `Latch`, `Queue`,
//! `Channel`, `PubSub`, `Ref`, `SynchronizedRef`, `SubscriptionRef` and `Supervisor`. Empty
//! until `c-coordination` fills it.
//!
//! Constructors are plain functions (not `make() -> Effect`), nothing here blocks a thread
//! (`G-nb` is clean on this directory), and nothing uses `tokio::sync::watch`.
