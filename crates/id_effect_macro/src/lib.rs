//! Declarative macros for the `effect` crate.
#![allow(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

/// Capability DI macros (`define_capability!`, `caps!`, `provide!`, `require!`).
pub mod capability;
pub mod context;
pub mod layer;
pub mod pipe;
pub mod service;
