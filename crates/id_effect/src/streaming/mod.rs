//! Streams: a pull-based, chunked [`Stream`], its consumers
//! ([`Sink`]), producer-fed streams ([`stream_from_channel`] / [`StreamSender`]), and the
//! stream-, sink- and fold-backed channels ([`StreamChannel`], [`FoldChannel`],
//! `Channel::to_stream` / `to_sink`).
//!
//! | 0.6 (`id_effect::streaming`) | 0.7 |
//! |---|---|
//! | `Arc<Mutex<StreamState>>`, locked on every pull | an owned pull: the rest of the stream is returned with each chunk, no lock |
//! | `poll_next_chunk(&mut self, &mut env)` | [`Stream::next_chunk`]`(self)` → `Effect<Option<(Chunk, Stream)>>` |
//! | `map_effect`: concurrent within a chunk (admission budget), `Or<E, E2>` errors, one `R` clone per element | [`Stream::map_effect`]: sequential, same `E`, no `R` clone |
//! | — | [`Stream::map_effect_par`]`(n, f)`: at most `n` in flight, ordered output |
//! | `StreamSender::fail` spinning on a full queue | [`StreamSender::fail`] never waits (failure slot + queue shutdown) |
//! | `Channel::from_stream` / `from_fold` / `from_sink` | [`StreamChannel`] / [`FoldChannel::from_fold`] / [`FoldChannel::from_sink`] |
//! | `with_throughput_metric(Metric)` | [`Stream::with_throughput_counter`]`(Arc<AtomicU64>)` |
//!
//! Every operator pulls one upstream chunk per downstream pull, so memory stays proportional to
//! the chunk size (plus documented operator state) and infinite sources work. [`Chunk`] and
//! [`Transducer`] are kernel-independent.
//!
//! ```
//! use id_effect::{Stream, succeed};
//! use id_effect::Never;
//!
//! // Sequential effectful map, then a bounded-concurrency one.
//! let out = Stream::<u32, Never>::from_iterable(1..=5)
//!   .map_effect(|n| succeed(n * 10))
//!   .map_effect_par(2, |n| succeed(n + 1))
//!   .run_collect()
//!   .block_on()
//!   .into_result();
//! assert_eq!(out.ok(), Some(vec![11, 21, 31, 41, 51]));
//! ```

mod broadcast;
mod channel;
mod join;
mod sender;
mod sink;
mod stream;
mod transduce;
mod window;

pub mod chunk;
pub mod transducer;

pub use broadcast::{StreamBroadcastFanout, broadcast_with_replay};
pub use channel::{FoldChannel, StreamChannel};
pub use chunk::Chunk;
pub use join::{combine_latest, keyed_join};
pub use sender::{
  BackpressureDecision, BackpressurePolicy, StreamChannelFull, StreamSender, backpressure_decision,
  end_stream, send_chunk, stream_from_channel, stream_from_channel_with_policy,
};
pub use sink::Sink;
pub use stream::{DEFAULT_CHUNK_SIZE, Stream};
pub use transduce::{StateStep, state_scan};
pub use transducer::{
  Reducer, Transducer, filter as transducer_filter, map as transducer_map, take as transducer_take,
};
#[doc(hidden)]
pub use window::window_instant;
