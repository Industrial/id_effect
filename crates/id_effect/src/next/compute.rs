//! The Compute Fabric as the fiber admission scheduler (ADR 0007 Amendment 1, D6). Empty until
//! `c-fabric-scheduler` fills it.
//!
//! `Concurrency::Inherit` awaits an asynchronous admission permit from the ambient Fabric before
//! each child of `fork_all` starts; `Concurrency::Bounded(n)` stays a purely local bound. No
//! blocking acquisition (`acquire_blocking`) is used from `next`.
