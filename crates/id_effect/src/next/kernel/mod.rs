//! The effect kernel: `Effect`, its constructors and combinators, the erased runloop, typed binds,
//! fork/resource combinators, the run edges and `LocalEffect`.

mod anybox;
mod bind;
mod bind_same;
mod cause;
mod constructors;
mod ctx;
mod edge;
mod effect;
mod fork;
mod local;
#[doc(hidden)]
pub mod macro_support;
mod resource;
mod runloop;

#[allow(unused_imports)] // filled by k-core-runloop
pub use anybox::*;
#[allow(unused_imports)] // filled by k-typed-r
pub use bind::*;
#[allow(unused_imports)] // filled by k-macros-next
pub use bind_same::*;
#[allow(unused_imports)] // filled by k-core-runloop
pub use cause::*;
#[allow(unused_imports)] // filled by k-core-runloop
pub use constructors::*;
#[allow(unused_imports)] // filled by k-core-runloop
pub use ctx::*;
#[allow(unused_imports)] // filled by k-edge
pub use edge::*;
#[allow(unused_imports)] // filled by k-core-runloop
pub use effect::*;
#[allow(unused_imports)] // filled by k-fiber-tree
pub use fork::*;
#[allow(unused_imports)] // filled by k-edge
pub use local::*;
#[allow(unused_imports)] // filled by k-fiber-tree
pub use resource::*;
#[allow(unused_imports)] // filled by k-core-runloop
pub use runloop::*;
