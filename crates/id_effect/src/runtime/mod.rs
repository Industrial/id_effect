//! Where effects run: the `Executor` trait and its tokio backend, fiber state and handles, fiber
//! refs, and the `Runtime` edges.

mod executor;
mod fiber;
mod fiber_context;
mod fiber_ref;
#[cfg(feature = "fiber-registry")]
mod fiber_registry;
#[allow(clippy::module_inception)]
mod runtime;
mod tokio;

pub use executor::*;
pub use fiber::*;
pub use fiber_context::*;
pub use fiber_ref::*;
#[cfg(feature = "fiber-registry")]
pub use fiber_registry::{FiberInfo, FiberState, fiber_dump};
pub use runtime::*;
// `self::` because the extern crate `tokio` is also in scope; inside `tokio.rs`, name the crate
// as `::tokio`.
pub use self::tokio::*;
