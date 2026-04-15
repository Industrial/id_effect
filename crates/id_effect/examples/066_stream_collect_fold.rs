//! Ex 066 — `run_collect` vs `run_fold`.
use id_effect::{Stream, run_blocking};

fn main() {
  let v = run_blocking(Stream::range(1, 5).run_collect(), ());
  assert_eq!(v, Ok(vec![1, 2, 3, 4]));
  let sum = run_blocking(
    Stream::from_iterable([1_i32, 2, 3]).run_fold(0, |a, b| a + b),
    (),
  );
  assert_eq!(sum, Ok(6));
  println!("066_stream_collect_fold ok");
}
