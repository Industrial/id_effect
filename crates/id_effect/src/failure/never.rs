//! [`Never`] — the uninhabited error type of infallible effects (Effect.ts `never`).
//!
//! Kernel-independent: shared by both kernels (`k-retire-v1` moved it here from `runtime`).

/// Effect.ts-style uninhabited error marker for infallible operations (an alias of
/// [`Infallible`](core::convert::Infallible)).
pub type Never = core::convert::Infallible;
