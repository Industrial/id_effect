//! Re-exports of the temporary next-kernel macros (`effect_next!`, `effect_local_next!`,
//! `caps_next!`) from `id_effect_proc_macro`. They are token-level proc-macros (`caps_next!`
//! compares capabilities by token, which `macro_rules!` cannot), coexist with the v1 `effect!` /
//! `caps!`, and are renamed by `k-retire-sweep`. Generic-helper narrowing (H17):
//! `kernel/bind_same.rs`.

pub use id_effect_proc_macro::{caps_next, effect_local_next, effect_next};
