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

#[allow(unused_imports)] // crate-internal items only (`crate::next::kernel::AnyBox` etc.)
pub use anybox::*;
#[allow(unused_imports)] // filled by k-typed-r
pub use bind::*;
#[allow(unused_imports)] // filled by k-macros-next
pub use bind_same::*;
pub use cause::*;
pub use constructors::*;
pub use ctx::*;
#[allow(unused_imports)] // filled by k-edge
pub use edge::*;
pub use effect::*;
#[allow(unused_imports)] // inherent impls only (`Effect::fork` / `fork_daemon`)
pub use fork::*;
#[allow(unused_imports)] // filled by k-edge
pub use local::*;
pub use resource::*;
pub use runloop::*;
