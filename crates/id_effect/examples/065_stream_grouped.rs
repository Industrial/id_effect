//! Ex 065 — `grouped` batches elements into fixed-size vectors.
use id_effect::{Stream, run_blocking};

fn main() {
  let s = Stream::range(1, 7).grouped(2).run_collect();
  assert_eq!(
    run_blocking(s, ()),
    Ok(vec![vec![1, 2], vec![3, 4], vec![5, 6]])
  );
  println!("065_stream_grouped ok");
}
