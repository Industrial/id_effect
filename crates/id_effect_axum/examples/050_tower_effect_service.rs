//! Minimal [`id_effect_axum::service::EffectService`]: map a request to an
//! [`id_effect::Effect`] and run it as a fiber inside Tower’s [`tower::Service::call`].
//!
//! The response future is `Send`, so any runtime flavour works; this example uses
//! `current_thread` to show that no worker pool is required.
//!
//! Run: `cargo run -p id_effect_axum --features tower --example 050_tower_effect_service`

use id_effect::succeed;
use id_effect_axum::service::EffectService;
use tower::{Service, ServiceExt};

#[tokio::main(flavor = "current_thread")]
async fn main() {
  let mut svc = EffectService::new((), |_env: &mut (), x: u32| {
    succeed::<u32, (), _>(x.saturating_add(1))
  });

  let out = svc.ready().await.unwrap().call(41).await.unwrap();
  assert_eq!(out, 42);
  println!("050_tower_effect_service: ok (41 + 1 = {out})");
}
