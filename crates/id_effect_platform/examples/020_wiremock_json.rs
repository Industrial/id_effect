//! [`id_effect_platform::http::reqwest::json`] with a typed response body.
//!
//! Run: `cargo run -p id_effect_platform --example 020_wiremock_json`

use id_effect::Exit;
use id_effect::hlist;
use id_effect_platform::http::reqwest::{ReqwestClient, json};
use id_effect_tokio::TokioRuntime;
use reqwest::{Client, Error};
use serde::{Deserialize, Serialize};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct Msg {
  n: i32,
}

#[tokio::main]
async fn main() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/data"))
    .respond_with(ResponseTemplate::new(200).set_body_json(&Msg { n: 7 }))
    .mount(&server)
    .await;

  let url = format!("{}/data", server.uri());
  // The capability can also be provided directly as an environment value.
  let program = json::<Msg, Error, _, _, Msg>(move |c| c.get(url))
    .provide_all(hlist![ReqwestClient(Client::new())]);
  let rt = TokioRuntime::current().expect("tokio runtime");
  let Exit::Success(msg) = rt.run(program).await else {
    panic!("request failed")
  };
  assert_eq!(msg, Msg { n: 7 });
  println!("020_wiremock_json ok");
}
