//! Per-request, [`EffectService`] clones handler state (`S: Clone`). To share mutable state across
//! calls, keep it behind something like [`std::sync::Arc`] (here an [`std::sync::atomic::AtomicU32`]).
//!
//! Service futures are `Send` and work on any Tokio runtime flavour (see
//! `050_tower_effect_service.rs`).
//!
//! Run: `cargo run -p id_effect_axum --features tower --example 051_tower_shared_counter`

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use id_effect::succeed;
use id_effect_axum::service::EffectService;
use tower::{Service, ServiceExt};

#[tokio::main(flavor = "current_thread")]
async fn main() {
  let hits = Arc::new(AtomicU32::new(0));
  let mut svc = EffectService::new(
    hits.clone(),
    |env: &mut Arc<AtomicU32>, label: &'static str| {
      let n = env.fetch_add(1, Ordering::Relaxed) + 1;
      succeed::<String, (), _>(format!("{label} #{n}"))
    },
  );

  let a = svc.ready().await.unwrap().call("req").await.unwrap();
  let b = svc.ready().await.unwrap().call("req").await.unwrap();
  assert_eq!(a, "req #1");
  assert_eq!(b, "req #2");
  assert_eq!(hits.load(Ordering::Relaxed), 2);
  println!("051_tower_shared_counter: ok ({a}, {b})");
}
