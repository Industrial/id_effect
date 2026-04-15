//! Ex 074 — `stream_from_channel_with_policy` selects queue backpressure mode.
use id_effect::{
  BackpressurePolicy, Chunk, end_stream, run_blocking, send_chunk, stream_from_channel_with_policy,
};

fn main() {
  let (stream, sender) =
    stream_from_channel_with_policy::<i32, (), ()>(4, BackpressurePolicy::DropNewest);
  assert_eq!(
    run_blocking(send_chunk(&sender, Chunk::from_vec(vec![1, 2, 3])), ()),
    Ok(())
  );
  assert_eq!(run_blocking(end_stream(sender), ()), Ok(()));
  let v = run_blocking(stream.run_collect(), ()).expect("collect");
  assert!(!v.is_empty());
  println!("074_stream_channel_policy ok");
}
