//! GET a URL via [`id_effect_platform::http`], provided by the reqwest layer and run on Tokio
//! ([`id_effect_tokio::TokioRuntime`]).

use id_effect::{Cause, Exit};
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{HttpClientEnv, HttpRequest, ReqwestHttpClientProvider, execute};
use id_effect_tokio::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), HttpError> {
  let url = std::env::args()
    .nth(1)
    .unwrap_or_else(|| "https://example.com".to_string());
  let program = execute::<HttpClientEnv, _>(HttpRequest::get(url))
    .provide_layer(ReqwestHttpClientProvider::layer());
  let rt = TokioRuntime::current().expect("tokio runtime");
  let res = match rt.run(program).await {
    Exit::Success(res) => res,
    Exit::Failure(Cause::Fail(e)) => return Err(e),
    Exit::Failure(c) => panic!("run failed: {c:?}"),
  };
  println!("status={} bytes={}", res.status, res.body.len());
  Ok(())
}
