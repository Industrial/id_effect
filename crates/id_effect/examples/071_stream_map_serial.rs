//! Ex 071 — `map_serial` takes an `FnMut` closure with mutable state.
use id_effect::Exit;
use id_effect::Stream;

fn main() {
  let mut offset = 0_i32;
  let s = Stream::<i32, ()>::from_iterable(1..=8).map_serial(move |n| {
    offset += 1;
    n + offset
  });
  assert_eq!(
    s.run_collect().block_on(),
    Exit::succeed(vec![2, 4, 6, 8, 10, 12, 14, 16])
  );
  println!("071_stream_map_serial ok");
}
