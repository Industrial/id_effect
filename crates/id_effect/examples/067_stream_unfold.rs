//! Ex 067 — `Stream::unfold` expands a state into elements.
use id_effect::Exit;
use id_effect::next::Stream;

fn main() {
  let s = Stream::<i32, ()>::unfold(1, |n| if n > 4 { None } else { Some((n, n + 1)) });
  assert_eq!(s.run_collect().block_on(), Exit::succeed(vec![1, 2, 3, 4]));
  println!("067_stream_unfold ok");
}
