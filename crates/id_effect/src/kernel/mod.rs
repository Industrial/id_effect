//! Stratum 2 — **Core Effect**: [`Effect`] and parallel binds.
//!
//! The [`effect`] submodule holds [`Effect`](id_effect::Effect), [`BoxFuture`](id_effect::BoxFuture), and
//! [`IntoBind`](id_effect::IntoBind). The pure `thunk`, `result` and `reader` helpers live in the
//! maintenance-tier `id_effect_fp` crate (`id_effect_fp::{thunk, result, reader}`).

pub mod effect;
pub mod par_bind;

pub use self::effect::{
  BoxFuture, Effect, IntoBind, acquire_release, box_future, fail, from_async, into_bind, pure,
  scope_with, scoped, succeed, unwrap_infallible,
};
pub use self::par_bind::{flatten_or, join_binds2, join_binds3, join_binds4};
