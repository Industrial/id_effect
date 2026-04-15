//! Ex 076 — `Stream::from_duplex_queue_channel` drains a [`QueueChannel`] with identical in/out type.
//!
//! Writes values, then [`QueueChannel::shutdown`] so [`Stream::run_collect`] can finish.
use id_effect::{QueueChannel, Stream, run_blocking};

fn main() {
  let ch = run_blocking(QueueChannel::<i32, i32, ()>::duplex_unbounded(), ()).expect("duplex");
  run_blocking(ch.write(1), ()).unwrap();
  run_blocking(ch.write(2), ()).unwrap();
  run_blocking(ch.shutdown(), ()).unwrap();
  let out = run_blocking(Stream::from_duplex_queue_channel(ch).run_collect(), ()).expect("collect");
  assert_eq!(out, vec![1, 2]);
  println!("076_stream_duplex_queue ok");
}
