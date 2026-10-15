//! The temporary v1 ↔ `next` bridge for crates that have not switched yet (release plan §3b).
//! Empty until `c-compat-bridge` fills it.
//!
//! `IntoV1` turns a `next::Effect<A, E, HNil>` into a v1 `Effect` that awaits the `next`
//! runtime; `IntoNext` turns a v1 `Effect` into a `next::LocalEffect` driven by `spawn_local`.
//! Every item is `#[doc(hidden)]`. Reached as `id_effect::next::compat::*` only (not glob
//! re-exported from `next`), so `k-retire-v1` can assert `rg 'next::compat'` = 0 and delete it.
