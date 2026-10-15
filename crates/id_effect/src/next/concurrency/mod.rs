//! Structured concurrency on fibers: `fork_all`, `Concurrency` and `Admission`.

mod fork_all;

#[allow(unused_imports)] // filled by k-fiber-tree
pub use fork_all::*;
