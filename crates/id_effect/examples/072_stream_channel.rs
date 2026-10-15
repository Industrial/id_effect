//! Ex 072 — `stream_from_channel` + `send_chunk` + `end_stream`.
use id_effect::{Chunk, Effect, StreamChannelFull, end_stream, send_chunk, stream_from_channel};
use id_effect::{Exit, Never};

fn main() {
  let (stream, sender) = stream_from_channel::<i32, &'static str, _>(3);
  let send: Effect<(), StreamChannelFull> = send_chunk(&sender, Chunk::from_vec(vec![1, 2]));
  assert!(send.block_on().into_result().is_ok());
  let send: Effect<(), StreamChannelFull> = send_chunk(&sender, Chunk::from_vec(vec![3, 4]));
  assert!(send.block_on().into_result().is_ok());
  let end: Effect<(), Never> = end_stream(sender);
  assert_eq!(end.block_on(), Exit::succeed(()));
  assert_eq!(
    stream.run_collect().block_on(),
    Exit::succeed(vec![1, 2, 3, 4])
  );
  println!("072_stream_channel ok");
}
