//! GET + [`id_effect_platform::http::reqwest::text`] against a local [wiremock](https://docs.rs/wiremock) server, with
//! the client provided by [`provide_reqwest_client`] and the effect run on
//! [`id_effect_tokio::TokioRuntime`].
//!
//! Run: `cargo run -p id_effect_platform --example 010_wiremock_get_text`

use id_effect::Exit;
use id_effect_platform::http::reqwest::{ReqwestClientEnv, provide_reqwest_client, text};
use id_effect_tokio::TokioRuntime;
use reqwest::{Client, Error};
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
  let program = text::<String, Error, ReqwestClientEnv, _>(move |c| c.get(url))
    .provide_layer(provide_reqwest_client(Client::new()));
  let rt = TokioRuntime::current().expect("tokio runtime");
  let Exit::Success(body) = rt.run(program).await else {
    panic!("request failed")
  };
  assert_eq!(body, "pong");
  println!("010_wiremock_get_text ok");
}
