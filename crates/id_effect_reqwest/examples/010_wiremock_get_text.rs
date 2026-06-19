//! GET + [`id_effect_reqwest::text`] against a local [wiremock](https://docs.rs/wiremock) server, driven
//! with [`id_effect_tokio::run_async`].
//!
//! Run: `cargo run -p id_effect_reqwest --example 010_wiremock_get_text`

use id_effect::build_env;
use id_effect_reqwest::{Client, Error, provide_reqwest_client, text};
use id_effect_tokio::run_async;
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
  let env = build_env([provide_reqwest_client(Client::new())]).expect("env");
  let body = run_async(text::<String, Error, _, _>(move |c| c.get(url)), env)
    .await
    .unwrap();
  assert_eq!(body, "pong");
  println!("010_wiremock_get_text ok");
}
