//! Declarative (`macro_rules!`) macros re-exported by `id_effect`.
//!
//! Expansions refer to `::id_effect::…`, so use them through `id_effect` (e.g. `id_effect::pipe!`).
//! The procedural `effect!` macro lives in `id_effect_proc_macro`.
#![allow(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

/// Capability DI macros (`caps!`, `provide!`, `providers!`, `require!`).
pub mod capability;
pub mod err;
pub mod pipe;
