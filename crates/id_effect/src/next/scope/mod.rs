//! Scopes: finalizer registries that outlive a single effect, and forking into them.

#[allow(clippy::module_inception)]
mod scope;

pub use scope::*;
