//! Build a [`reqwest::Client`] with the [`ReqwestClientLive`] layer and run an HTTP effect
//! against it (the SSRF guard allowlists the local mock server, `127.0.0.1`).
//!
//! Run: `cargo run -p id_effect_platform --example 030_layer_default_env`

use id_effect::Exit;
use id_effect_platform::http::HttpClientConfig;
use id_effect_platform::http::reqwest::{ReqwestClientEnv, ReqwestClientLive, text};
use id_effect_tokio::TokioRuntime;
use reqwest::Error;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::main]
async fn main() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/layer"))
    .respond_with(ResponseTemplate::new(200).set_body_string("layer-ok"))
    .mount(&server)
    .await;

  let url = format!("{}/layer", server.uri());
  let program = text::<String, Error, ReqwestClientEnv, _>(move |c| c.get(url)).provide_layer(
    ReqwestClientLive::layer_with(HttpClientConfig::default().allow_hosts(["127.0.0.1"])),
  );
  let rt = TokioRuntime::current().expect("tokio runtime");
  let Exit::Success(body) = rt.run(program).await else {
    panic!("request failed")
  };
  assert_eq!(body, "layer-ok");
  println!("030_layer_default_env ok");
}
