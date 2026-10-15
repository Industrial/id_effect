// `p-layer-scoped`: `derive(ProviderSpec)` emits `layer()` on the `id_effect::next` path, next to
// the v1 `ProviderSpec` impl. With `release = ..` the layer carries an async finalizer that runs
// when the scope of `provide_layer` closes, after the effect.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use id_effect::Never;
use id_effect::next::{Effect, HCons, HNil, need, sync};
use id_effect::{Exit, ProviderSpec, ProviderSpecDerive};

#[derive(Clone)]
pub struct Counter(u32);

#[derive(ProviderSpecDerive)]
#[provides(Counter, with = || Counter(42))]
pub struct CounterLive;

static CLOSED: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
pub struct Pool(&'static str);

fn close_pool(pool: Arc<Pool>) -> Effect<(), Never, HNil> {
  sync(move || {
    assert_eq!(pool.0, "mem://");
    CLOSED.store(true, Ordering::SeqCst);
  })
}

#[derive(ProviderSpecDerive)]
#[provides(Pool, with = || Pool("mem://"), release = close_pool)]
pub struct PoolLive;

fn read_counter() -> Effect<u32, (), HCons<Counter, HNil>> {
  need::<Counter>().map(|c| c.0)
}

fn use_pool() -> Effect<bool, (), HCons<Pool, HNil>> {
  need::<Pool>().map(|_| CLOSED.load(Ordering::SeqCst))
}

fn main() {
  // The v1 emission still works.
  assert_eq!(CounterLive::provider_id(), "counter-live");

  // The next emission: `layer()` (any error type, inferred here as `()`).
  let read: Effect<u32, ()> = read_counter().provide_layer(CounterLive::layer());
  assert!(matches!(read.block_on(), Exit::Success(42)));

  let used: Effect<bool, ()> = use_pool().provide_layer(PoolLive::layer());
  assert!(
    matches!(used.block_on(), Exit::Success(false)),
    "open while used"
  );
  assert!(CLOSED.load(Ordering::SeqCst), "released after the effect");
}
