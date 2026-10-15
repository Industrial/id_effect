//! The 0.7 kernel (design A, ADR 0011), landed side by side with the v1 kernel.
//!
//! Hidden from the docs and purely additive until `k-retire-sweep` renames `id_effect::next::` to
//! `id_effect::`. Every submodule's items are glob re-exported here, so item names must be unique
//! across submodules (`ambiguous_glob_reexports` is denied by clippy `-D warnings`).
//!
//! Shared, kernel-independent types come from the crate root: `crate::failure::{Cause, Defect,
//! Exit}`, `crate::runtime::{FiberId, Never}`.

pub mod capability;
pub mod concurrency;
pub mod kernel;
pub mod layer;
pub mod macros;
pub mod runtime;
pub mod scope;
pub mod testing;

#[allow(unused_imports)] // filled by k-core-runloop (hlist)
pub use capability::*;
#[allow(unused_imports)] // filled by k-fiber-tree
pub use concurrency::*;
pub use kernel::*;
#[allow(unused_imports)] // filled by k-layer-base
pub use layer::*;
#[allow(unused_imports)] // filled by k-macros-next
pub use macros::*;
pub use runtime::*;
#[allow(unused_imports)] // filled by k-scope-fiberref
pub use scope::*;
pub use testing::*;

#[doc(hidden)]
pub use kernel::macro_support as __private;
