#![cfg(feature = "cursor")]

use id_effect::run_async;
use id_effect_ai::{AiConfig, CreateAgentRequest, CursorAgentsClient, HttpCursorAgentsClient};
use id_effect_config::Secret;
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpClient, HttpMethod, HttpRequest, HttpResponse, StreamingHttpResponse,
};
use std::sync::Arc;

#[derive(Clone, Default)]
struct MockHttpClient;

impl HttpClient for MockHttpClient {
  fn execute(&self, req: HttpRequest) -> id_effect::Effect<HttpResponse, HttpError, ()> {
    let body = if req.url.contains("/v1/models") {
      br#"{"items":[{"id":"composer-2","displayName":"Composer 2"}]}"#.to_vec()
    } else if req.url.ends_with("/v1/agents") && matches!(req.method, HttpMethod::Post) {
      br#"{"agent":{"id":"bc-test","name":"t","status":"ACTIVE","latestRunId":"run-1"},"run":{"id":"run-1","agentId":"bc-test","status":"CREATING"}}"#.to_vec()
    } else if req.url.contains("/runs/run-1") {
      br#"{"id":"run-1","agentId":"bc-test","status":"FINISHED"}"#.to_vec()
    } else if req.url.contains("/v1/agents/bc-test") {
      br#"{"id":"bc-test","name":"t","status":"ACTIVE","latestRunId":"run-1"}"#.to_vec()
    } else {
      b"{}".to_vec()
    };
    id_effect::Effect::new(move |_r| {
      Ok(HttpResponse {
        status: 200,
        headers: vec![],
        body,
      })
    })
  }

  fn execute_stream(
    &self,
    _req: HttpRequest,
  ) -> id_effect::Effect<StreamingHttpResponse, HttpError, ()> {
    id_effect::fail(HttpError::InvalidRequest(
      "stream not supported in mock".into(),
    ))
  }
}

fn fixture_config() -> AiConfig {
  AiConfig {
    cursor_api_key: Some(Secret::new("cursor-test".to_string())),
    cursor_base_url: "https://api.cursor.com".to_string(),
    ..AiConfig::default()
  }
}

#[tokio::test]
async fn cursor_list_models_fixture() {
  let client = Arc::new(MockHttpClient) as Arc<dyn HttpClient>;
  let agents = HttpCursorAgentsClient::new(client, fixture_config()).expect("client");
  let models = run_async(agents.list_models(), ()).await.expect("models");
  assert_eq!(models[0].id, "composer-2");
}

#[tokio::test]
async fn cursor_create_and_poll_fixture() {
  let client = Arc::new(MockHttpClient) as Arc<dyn HttpClient>;
  let agents = HttpCursorAgentsClient::new(client, fixture_config()).expect("client");
  let (agent, run) = run_async(
    agents.create_agent(CreateAgentRequest {
      prompt_text: "hello".into(),
      model_id: Some("composer-2".into()),
      repos: vec![],
    }),
    (),
  )
  .await
  .expect("create");
  assert_eq!(agent.id, "bc-test");
  assert_eq!(run.id, "run-1");
  let done = run_async(
    agents.wait_until_idle(&agent.id, &run.id, std::time::Duration::from_secs(2)),
    (),
  )
  .await
  .expect("poll");
  assert!(done.status.is_terminal());
}
