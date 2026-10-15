//! Ex 061 — `Stream::from_iterable` turns a finite iterator into a pull stream.
use id_effect::Exit;
use id_effect::Stream;

fn main() {
  let stream = Stream::<i32, ()>::from_iterable([1, 2, 3, 4]);
  assert_eq!(
    stream.run_collect().block_on(),
    Exit::succeed(vec![1, 2, 3, 4])
  );
  println!("061_stream_from_iterable ok");
}
