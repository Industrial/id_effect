//! Ex 074 — `stream_from_channel_with_policy` selects the queue backpressure mode.
use id_effect::{
  BackpressurePolicy, Chunk, Effect, StreamChannelFull, end_stream, send_chunk,
  stream_from_channel_with_policy,
};
use id_effect::{Exit, Never};

fn main() {
  let (stream, sender) =
    stream_from_channel_with_policy::<i32, (), _>(4, BackpressurePolicy::DropNewest);
  let send: Effect<(), StreamChannelFull> = send_chunk(&sender, Chunk::from_vec(vec![1, 2, 3]));
  assert!(send.block_on().into_result().is_ok());
  let end: Effect<(), Never> = end_stream(sender);
  assert_eq!(end.block_on(), Exit::succeed(()));
  let v = stream
    .run_collect()
    .block_on()
    .into_result()
    .expect("collect");
  assert!(!v.is_empty());
  println!("074_stream_channel_policy ok");
}
