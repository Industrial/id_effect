//! Ex 067 — `Stream::unfold` expands state into elements (sync `()`, `()` env).
use id_effect::{Stream, run_blocking};

fn main() {
  let s = Stream::unfold(1_i32, |n| if n > 4 { None } else { Some((n, n + 1)) });
  assert_eq!(run_blocking(s.run_collect(), ()), Ok(vec![1, 2, 3, 4]));
  println!("067_stream_unfold ok");
}
