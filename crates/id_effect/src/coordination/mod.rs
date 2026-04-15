//! **Stratum 9 — Coordination & Communication**
//!
//! Primitives for inter-fiber communication and synchronisation, built from Strata 0–8.
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`deferred`] | [`Deferred`] | Stratum 6 (`runtime`), Stratum 4 (`failure`) |
//! | [`latch`] | [`Latch`] | [`deferred`] |
//! | [`queue`] | [`Queue`], [`QueueError`] | Stratum 2 (`kernel`) |
//! | [`semaphore`] | [`Semaphore`], [`Permit`] | Stratum 8 (`resource::Scope`) |
//! | [`pubsub`] | [`PubSub`] | [`queue`], Stratum 8 (`resource::Scope`) |
//! | [`channel`] | [`Channel`], [`ChannelReadError`], [`QueueChannel`] | [`queue`], Stratum 11 |
//! | [`ref_`] | [`Ref`] | Stratum 2 (`kernel`) |
//! | [`synchronized_ref`] | [`SynchronizedRef`] | Stratum 2 (`kernel`) |
//!
//! ## Public API
//!
//! Re-exported at the crate root: all public types and functions from each submodule.

pub mod channel;
pub mod deferred;
pub mod latch;
pub mod pubsub;
pub mod queue;
pub mod ref_;
pub mod semaphore;
pub mod synchronized_ref;

pub use channel::{Channel, ChannelReadError, QueueChannel};
pub use deferred::Deferred;
pub use latch::Latch;
pub use pubsub::PubSub;
pub use queue::{Queue, QueueError};
pub use ref_::Ref;
pub use semaphore::{Permit, Semaphore};
pub use synchronized_ref::SynchronizedRef;
