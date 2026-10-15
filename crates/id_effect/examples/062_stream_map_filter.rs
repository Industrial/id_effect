//! Ex 062 — `map` / `filter` on streams.
use id_effect::Exit;
use id_effect::next::Stream;

fn main() {
  let s = Stream::<i64, ()>::range(1, 10)
    .map(|n| n * 2)
    .filter(|n: &i64| *n % 3 == 0)
    .run_collect();
  assert_eq!(s.block_on(), Exit::succeed(vec![6, 12, 18]));
  println!("062_stream_map_filter ok");
}
