//! Where effects run: the `Executor` trait and its tokio backend, fiber state and handles, fiber
//! refs, and the `Runtime` edges.

mod executor;
mod fiber;
mod fiber_context;
mod fiber_ref;
#[allow(clippy::module_inception)]
mod runtime;
mod tokio;

pub use executor::*;
#[allow(unused_imports)] // filled by k-fiber-tree
pub use fiber::*;
#[allow(unused_imports)] // filled by k-core-runloop
pub use fiber_context::*;
#[allow(unused_imports)] // filled by k-scope-fiberref
pub use fiber_ref::*;
#[allow(unused_imports)] // filled by k-edge
pub use runtime::*;
// `self::` because the extern crate `tokio` is also in scope; inside `tokio.rs`, name the crate
// as `::tokio`.
#[allow(unused_imports)] // filled by k-executor
pub use self::tokio::*;
