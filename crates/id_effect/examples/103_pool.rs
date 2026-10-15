//! Ex 103 — A `KeyedPool` reuses values per key; a checked-out value returns when its scope closes.
use id_effect::{Effect, KeyedPool, Scope, succeed};

fn main() {
  let pool = KeyedPool::new(2, |k: &'static str| succeed(k.len() as u32));
  let scope = Scope::new();
  let v: Effect<u32, id_effect::Never> = pool.get("answer", &scope);
  assert_eq!(v.block_on().into_result().ok(), Some(6));
  println!("103_pool ok");
}
