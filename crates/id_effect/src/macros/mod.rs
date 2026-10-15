//! The macro surface, re-exported at the crate root.
//!
//! - Procedural (`id_effect_proc_macro`): `effect!` / `effect_local!` (do-notation over an
//!   async body building an `Effect` / `LocalEffect`), `caps!` (the typed environment
//!   `HCons<A, HCons<B, HNil>>`). Generic-helper narrowing (H17): `kernel/bind_same.rs`.
//! - Declarative (`id_effect_macro`): `pipe!` (left-to-right application) and `err!`
//!   (type-level error union `Or<A, Or<B, C>>`).

pub use id_effect_macro::{err, pipe};
pub use id_effect_proc_macro::{caps, effect, effect_local};
