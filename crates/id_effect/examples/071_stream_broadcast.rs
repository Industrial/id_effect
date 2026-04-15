//! Ex 071 — `broadcast` fans one stream out through a `PubSub` hub (async `tokio::join!`).
use id_effect::{Stream, run_async};

#[tokio::main(flavor = "current_thread")]
async fn main() {
  let src = Stream::from_iterable(vec![1_i32, 2, 3]);
  let (streams, pump) = run_async(src.broadcast(8, 2), ()).await.expect("broadcast");
  assert_eq!(streams.len(), 2);
  let mut streams = streams;
  let s1 = streams.pop().unwrap();
  let s0 = streams.pop().unwrap();
  let (pr, a, b) = tokio::join!(
    run_async(pump, ()),
    run_async(s0.run_collect(), ()),
    run_async(s1.run_collect(), ()),
  );
  pr.expect("pump");
  assert_eq!(a.expect("a"), vec![1, 2, 3]);
  assert_eq!(b.expect("b"), vec![1, 2, 3]);
  println!("071_stream_broadcast ok");
}
