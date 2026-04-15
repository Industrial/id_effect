//! Ex 061 — `Stream::from_iterable` materializes a finite iterator into a pull stream.
use id_effect::{Stream, run_blocking};

fn main() {
  let stream = Stream::from_iterable([1_i32, 2, 3, 4]);
  let out = run_blocking(stream.run_collect(), ()).expect("run_collect");
  assert_eq!(out, vec![1, 2, 3, 4]);
  println!("061_stream_from_iterable ok");
}
