//! Ex 063 — `take`, `take_while`, `drop_while`.
use id_effect::Exit;
use id_effect::next::Stream;

fn main() {
  let s = Stream::<i64, ()>::range(0, 100)
    .take(4)
    .take_while(|n: &i64| *n < 3)
    .run_collect();
  assert_eq!(s.block_on(), Exit::succeed(vec![0, 1, 2]));

  let s2 = Stream::<i64, ()>::range(0, 10)
    .drop_while(|n: &i64| *n < 7)
    .take(2)
    .run_collect();
  assert_eq!(s2.block_on(), Exit::succeed(vec![7, 8]));
  println!("063_stream_take_drop ok");
}
