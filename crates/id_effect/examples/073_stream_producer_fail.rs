//! Ex 073 — A producer `fail` surfaces on `run_collect`.
use id_effect::next::stream_from_channel;
use id_effect::{Cause, Exit};

fn main() {
  let (stream, sender) = stream_from_channel::<i32, &'static str, _>(1);
  assert!(sender.fail("boom"));
  assert_eq!(
    stream.run_collect().block_on(),
    Exit::Failure(Cause::Fail("boom"))
  );
  println!("073_stream_producer_fail ok");
}
