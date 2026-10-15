//! Ex 104 — A `Cache` memoizes lookups; `CacheStats` tracks hits and misses.
use id_effect::Never;
use id_effect::next::{Cache, CacheStats, Effect, succeed};

fn main() {
  let cache = Cache::new(8, None, |k: &'static str| succeed(k.len() as u64));
  let program: Effect<(u64, CacheStats, u64, CacheStats), Never> = cache
    .get("hi")
    .zip(cache.stats())
    .zip(cache.get("hi"))
    .zip(cache.stats())
    .map(|(((a, s1), b), s2)| (a, s1, b, s2));
  let (v, stats, v2, stats2) = program.block_on().into_result().expect("cache");
  assert_eq!((v, v2), (2, 2));
  assert_eq!((stats.misses, stats.hits), (1, 0));
  assert_eq!(stats2.hits, 1);
  println!("104_cache ok");
}
