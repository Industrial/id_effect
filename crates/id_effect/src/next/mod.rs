//! The 0.7 kernel (design A, ADR 0011), landed side by side with the v1 kernel.
//!
//! Hidden from the docs and purely additive until `k-retire-sweep` renames `id_effect::next::` to
//! `id_effect::`. Every submodule's items are glob re-exported here, so item names must be unique
//! across submodules (`ambiguous_glob_reexports` is denied by clippy `-D warnings`).
//!
//! Shared, kernel-independent types come from the crate root: `crate::failure::{Cause, Defect,
//! Exit}`, `crate::failure::{FiberId, Never}`.
//!
//! Modules that are still empty were scaffolded by `w3-scaffold` with their glob re-export in
//! place, so the leaf that fills one never has to touch this file. `compat` is the exception: it
//! is reached as `next::compat::*` only, so it is not glob re-exported.

pub mod capability;
pub mod compat;
pub mod compute;
pub mod concurrency;
pub mod coordination;
pub mod kernel;
pub mod layer;
pub mod macros;
pub mod observability;
pub mod par;
pub mod race;
pub mod random;
pub mod request;
pub mod resource;
pub mod runtime;
pub mod scheduling;
pub mod scope;
pub mod streaming;
pub mod testing;

pub use capability::*;
#[allow(unused_imports)] // filled by c-fabric-scheduler
pub use compute::*;
pub use concurrency::*;
#[allow(unused_imports)] // filled by c-coordination
pub use coordination::*;
pub use kernel::*;
pub use layer::*;
#[allow(unused_imports)] // filled by k-macros-next
pub use macros::*;
#[allow(unused_imports)] // filled by p-metrics
pub use observability::*;
#[allow(unused_imports)] // filled by c-zip-par-all
pub use par::*;
#[allow(unused_imports)] // filled by c-race-timeout
pub use race::*;
#[allow(unused_imports)] // filled by p-random
pub use random::*;
#[allow(unused_imports)] // filled by p-request-batch, p-request-cache
pub use request::*;
#[allow(unused_imports)] // filled by c-resource
pub use resource::*;
pub use runtime::*;
#[allow(unused_imports)] // filled by p-schedule
pub use scheduling::*;
pub use scope::*;
#[allow(unused_imports)] // filled by m-streams
pub use streaming::*;
pub use testing::*;

#[doc(hidden)]
pub use kernel::macro_support as __private;
