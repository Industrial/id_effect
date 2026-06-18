//! **Stratum 11 — Streaming**
//!
//! Lazy, chunked, effectful sequences, built from Strata 0–10.
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`chunk`] | [`Chunk`] | Stratum 0 |
//! | [`sink`] | [`Sink`] | [`chunk`], Stratum 9 (`coordination`) |
//! | [`stream`] | [`Stream`], [`StreamSender`], [`BackpressurePolicy`] | [`chunk`], [`sink`], Stratum 9 |
//! | [`window`] | tumbling / sliding / session windows | [`stream`] |
//! | [`join`] | merge, combine-latest, keyed join | [`stream`] |
//! | [`replay`] | replay-buffer broadcast fanout | [`stream`], Stratum 9 |
//! | [`state_scan`] | FSM-style optional-output scan | [`stream`] |
//! | [`transducer`] | via_transducer / transduce_items | [`stream`] |
//!
//! ## Public API
//!
//! Re-exported at the crate root: all public types and functions.

pub mod chunk;
pub mod join;
pub mod replay;
pub mod sink;
pub mod state_scan;
pub mod stream;
pub mod transducer;
pub mod window;

pub use chunk::Chunk;
pub use join::{combine_latest, keyed_join};
pub use replay::broadcast_with_replay;
pub use sink::Sink;
pub use state_scan::state_scan;
pub use stream::{
  BackpressureDecision, BackpressurePolicy, Stream, backpressure_decision, end_stream, send_chunk,
  stream_from_channel, stream_from_channel_with_policy,
};
pub use transducer::{Transducer, filter as transducer_filter, map as transducer_map};
