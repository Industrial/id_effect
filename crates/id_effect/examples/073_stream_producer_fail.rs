//! Ex 073 — Producer `fail` surfaces on `run_collect`.
use id_effect::{run_blocking, stream_from_channel};

fn main() {
  let (stream, sender) = stream_from_channel::<i32, &'static str, ()>(1);
  assert!(sender.fail("boom"));
  assert_eq!(run_blocking(stream.run_collect(), ()), Err("boom"));
  println!("073_stream_producer_fail ok");
}
