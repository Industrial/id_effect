//! Ex 071 — `broadcast` fans one stream out to several branches, driven by a pump effect.
use id_effect::Exit;
use id_effect::next::{Concurrency, Effect, Stream, all};

fn main() {
  let src = Stream::<i32, ()>::from_iterable(vec![1, 2, 3]);
  let program: Effect<Vec<Vec<i32>>, ()> = src.broadcast(8, 2).flat_map(|(streams, pump)| {
    let collectors: Vec<Effect<Vec<i32>, ()>> =
      streams.into_iter().map(|s| s.run_collect()).collect();
    pump
      .fork()
      .widen()
      .flat_map(move |_pump| all(collectors, Concurrency::Unbounded))
  });
  assert_eq!(
    program.block_on(),
    Exit::succeed(vec![vec![1, 2, 3], vec![1, 2, 3]])
  );
  println!("071_stream_broadcast ok");
}
