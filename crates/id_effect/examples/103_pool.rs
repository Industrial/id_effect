//! Ex 103 — `KeyedPool` reuses values per key under a global capacity cap.
use id_effect::{KeyedPool, Scope, run_async, succeed};

#[tokio::main(flavor = "current_thread")]
async fn main() {
  let pool = run_async(
    KeyedPool::make(2, |k: &'static str| succeed::<u32, (), ()>(k.len() as u32)),
    (),
  )
  .await
  .expect("pool");
  let scope = Scope::make();
  let v = run_async(pool.get("answer"), scope.clone())
    .await
    .expect("get");
  assert_eq!(v, 6);
  println!("103_pool ok");
}
