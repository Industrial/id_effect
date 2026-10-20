//! GET + [`id_effect_platform::http::reqwest::text`] against a local [wiremock](https://docs.rs/wiremock) server, with
//! the client provided by [`ReqwestClientLive::layer_with`] (the SSRF guard allowlists the
//! loopback mock) and the effect run on [`id_effect_tokio::TokioRuntime`].
//!
//! Run: `cargo run -p id_effect_platform --example 010_wiremock_get_text`

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
    .and(path("/ping"))
    .respond_with(ResponseTemplate::new(200).set_body_string("pong"))
    .mount(&server)
    .await;

  let url = format!("{}/ping", server.uri());
  let program = text::<String, Error, ReqwestClientEnv, _>(move |c| c.get(url)).provide_layer(
    ReqwestClientLive::layer_with(HttpClientConfig::default().allow_hosts(["127.0.0.1"])),
  );
  let rt = TokioRuntime::current().expect("tokio runtime");
  let Exit::Success(body) = rt.run(program).await else {
    panic!("request failed")
  };
  assert_eq!(body, "pong");
  println!("010_wiremock_get_text ok");
}
