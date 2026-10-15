//! Ex 066 — `run_collect` vs `run_fold`.
use id_effect::Exit;
use id_effect::next::Stream;

fn main() {
  let v = Stream::<i64, ()>::range(1, 5).run_collect().block_on();
  assert_eq!(v, Exit::succeed(vec![1, 2, 3, 4]));
  let sum = Stream::<i32, ()>::from_iterable([1, 2, 3])
    .run_fold(0, |a, b| a + b)
    .block_on();
  assert_eq!(sum, Exit::succeed(6));
  println!("066_stream_collect_fold ok");
}
