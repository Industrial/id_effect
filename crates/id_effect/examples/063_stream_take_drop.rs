//! Ex 063 — `take`, `take_while`, `drop_while`.
use id_effect::{Stream, run_blocking};

fn main() {
  let s = Stream::range(0, 100)
    .take(4)
    .take_while(Box::new(|n: &i64| *n < 3))
    .run_collect();
  assert_eq!(run_blocking(s, ()), Ok(vec![0, 1, 2]));

  let s2 = Stream::range(0, 10)
    .drop_while(Box::new(|n: &i64| *n < 7))
    .take(2)
    .run_collect();
  assert_eq!(run_blocking(s2, ()), Ok(vec![7, 8]));
  println!("063_stream_take_drop ok");
}
