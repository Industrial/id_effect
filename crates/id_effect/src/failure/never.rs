//! [`Never`] — the uninhabited error type of infallible effects (Effect.ts `never`).
//!
//! Kernel-independent (0.6 kept it in `runtime`).

/// Effect.ts-style uninhabited error marker for infallible operations (an alias of
/// [`Infallible`](core::convert::Infallible)).
pub type Never = core::convert::Infallible;
