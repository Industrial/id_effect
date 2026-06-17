//! GET a URL via [`id_effect_platform::http`] + Tokio (`id_effect_tokio::run_async`).

use id_effect::{RunError, provide, run_with};
use id_effect_platform::http::{HttpRequest, ReqwestHttpClientProvider, execute};

#[tokio::main]
async fn main() -> Result<(), id_effect_platform::error::HttpError> {
  let url = std::env::args()
    .nth(1)
    .unwrap_or_else(|| "https://example.com".to_string());
  let res = run_with(
    [provide!(ReqwestHttpClientProvider)],
    execute(HttpRequest::get(url)),
  )
  .map_err(|e| match e {
    RunError::Effect(e) => e,
    e => panic!("run failed: {e}"),
  })?;
  println!("status={} bytes={}", res.status, res.body.len());
  Ok(())
}
