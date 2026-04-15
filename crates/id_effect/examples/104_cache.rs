//! Ex 104 — `Cache` memoizes loads; `CacheStats` tracks hits/misses.
use id_effect::{Cache, run_async, run_blocking, succeed};

#[tokio::main(flavor = "current_thread")]
async fn main() {
  let cache = run_blocking(
    Cache::make(8, None, |k: &'static str| {
      succeed::<u64, (), ()>(k.len() as u64)
    }),
    (),
  )
  .expect("cache");
  let v = run_async(cache.get("hi"), ()).await.expect("get");
  assert_eq!(v, 2_u64);
  let stats = run_async(cache.stats(), ()).await.expect("stats");
  assert_eq!(stats.misses, 1);
  assert_eq!(stats.hits, 0);
  let v2 = run_async(cache.get("hi"), ()).await.expect("get2");
  assert_eq!(v2, 2);
  let stats2 = run_async(cache.stats(), ()).await.expect("stats2");
  assert_eq!(stats2.hits, 1);
  println!("104_cache ok");
}
