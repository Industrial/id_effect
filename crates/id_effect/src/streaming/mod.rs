//! **Stratum 11 — Streaming**
//!
//! Lazy, chunked, effectful sequences, built from Strata 0–10.
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`chunk`] | [`Chunk`] | Stratum 0 |
//! | [`sink`] | [`Sink`] | [`chunk`], Stratum 9 (`coordination`) |
//! | [`stream`] | [`Stream`], [`StreamSender`], [`BackpressurePolicy`] | [`chunk`], [`sink`], Stratum 9 |
//!
//! ## Public API
//!
//! Re-exported at the crate root: all public types and functions.

pub mod chunk;
pub mod sink;
pub mod stream;

pub use chunk::Chunk;
pub use sink::Sink;
pub use stream::{
  BackpressureDecision, BackpressurePolicy, Stream, backpressure_decision, end_stream, send_chunk,
  stream_from_channel, stream_from_channel_with_policy,
};
