//! Ex 062 — `map` / `filter` on streams.
use id_effect::{Stream, run_blocking};

fn main() {
  let s = Stream::range(1, 10)
    .map(|n| n * 2)
    .filter(Box::new(|n: &i64| *n % 3 == 0))
    .run_collect();
  assert_eq!(run_blocking(s, ()), Ok(vec![6, 12, 18]));
  println!("062_stream_map_filter ok");
}
