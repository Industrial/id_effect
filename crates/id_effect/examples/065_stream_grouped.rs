//! Ex 065 — `grouped` batches elements into fixed-size vectors.
use id_effect::Exit;
use id_effect::next::Stream;

fn main() {
  let s = Stream::<i64, ()>::range(1, 7).grouped(2).run_collect();
  assert_eq!(
    s.block_on(),
    Exit::succeed(vec![vec![1, 2], vec![3, 4], vec![5, 6]])
  );
  println!("065_stream_grouped ok");
}
