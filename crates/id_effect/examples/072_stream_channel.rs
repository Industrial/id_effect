//! Ex 072 — `stream_from_channel` + `send_chunk` + `end_stream`.
use id_effect::{Chunk, end_stream, run_blocking, send_chunk, stream_from_channel};

fn main() {
  let (stream, sender) = stream_from_channel::<i32, &'static str, ()>(3);
  assert_eq!(
    run_blocking(send_chunk(&sender, Chunk::from_vec(vec![1, 2])), ()),
    Ok(())
  );
  assert_eq!(
    run_blocking(send_chunk(&sender, Chunk::from_vec(vec![3, 4])), ()),
    Ok(())
  );
  assert_eq!(run_blocking(end_stream(sender), ()), Ok(()));
  assert_eq!(run_blocking(stream.run_collect(), ()), Ok(vec![1, 2, 3, 4]));
  println!("072_stream_channel ok");
}
