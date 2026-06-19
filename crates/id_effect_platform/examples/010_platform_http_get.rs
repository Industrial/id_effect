//! GET a URL via [`id_effect_platform::http`] + Tokio (`id_effect::run_async`).

use id_effect::{Env, RunError, run_with};
use id_effect_platform::http::{HttpRequest, execute, provide_reqwest_http_client};

#[tokio::main]
async fn main() -> Result<(), id_effect_platform::error::HttpError> {
  let url = std::env::args()
    .nth(1)
    .unwrap_or_else(|| "https://example.com".to_string());
  let res = run_with(
    [provide_reqwest_http_client()],
    execute::<Env>(HttpRequest::get(url)),
  )
  .map_err(|e| match e {
    RunError::Effect(e) => e,
    e => panic!("run failed: {e}"),
  })?;
  println!("status={} bytes={}", res.status, res.body.len());
  Ok(())
}
