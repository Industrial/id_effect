//! Scopes: finalizer registries that outlive a single effect, and forking into them.

#[allow(clippy::module_inception)]
mod scope;

#[allow(unused_imports)] // filled by k-scope-fiberref
pub use scope::*;
