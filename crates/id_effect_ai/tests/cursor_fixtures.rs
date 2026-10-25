//! Cursor Cloud Agents client against fixture responses (D6: kept behind `cursor`).
#![cfg(feature = "cursor")]

use std::sync::Arc;
use std::time::Duration;

use id_effect_ai::{
  AiConfig, AiError, CreateAgentRequest, CursorAgentsClient, CursorAgentsClientService, CursorRepo,
  HttpCursorAgentsClient, provide_cursor_agents_client,
};
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpBody, HttpClient, HttpMethod, HttpRequest, HttpResponse, StreamingHttpResponse,
};
use id_effect_tokio::TokioRuntime;

/// Answers the Cursor endpoints with fixtures; `status` replaces 200 for every call, and a run
/// reports `run_status`.
#[derive(Clone)]
struct MockHttpClient {
  status: u16,
  run_status: &'static str,
  transport_error: Option<HttpError>,
}

impl Default for MockHttpClient {
  fn default() -> Self {
    Self {
      status: 200,
      run_status: "FINISHED",
      transport_error: None,
    }
  }
}

impl HttpClient for MockHttpClient {
  fn execute(&self, req: HttpRequest) -> id_effect::Effect<HttpResponse, HttpError> {
    if let Some(e) = self.transport_error.clone() {
      return id_effect::fail(e);
    }
    assert!(req.timeout.is_some(), "every request carries a timeout");
    assert!(
      req.max_body_bytes.is_some(),
      "every request carries a body cap"
    );
    let body = if req.url.contains("/v1/models") {
      br#"{"items":[{"id":"composer-2","displayName":"Composer 2"}]}"#.to_vec()
    } else if req.url.ends_with("/v1/agents") && matches!(req.method, HttpMethod::Post) {
      br#"{"agent":{"id":"bc-test","name":"t","status":"ACTIVE","latestRunId":"run-1"},"run":{"id":"run-1","agentId":"bc-test","status":"CREATING"}}"#.to_vec()
    } else if req.url.ends_with("/runs") && matches!(req.method, HttpMethod::Post) {
      br#"{"run":{"id":"run-2","agentId":"bc-test","status":"RUNNING"}}"#.to_vec()
    } else if req.url.contains("/runs/") {
      format!(
        r#"{{"id":"run-1","agentId":"bc-test","status":"{}"}}"#,
        self.run_status
      )
      .into_bytes()
    } else if req.url.contains("/v1/agents/bc-test") {
      br#"{"id":"bc-test","name":"t","status":"ACTIVE","latestRunId":"run-1"}"#.to_vec()
    } else {
      b"{}".to_vec()
    };
    id_effect::succeed(HttpResponse {
      status: self.status,
      headers: vec![],
      body,
    })
  }

  fn execute_stream(
    &self,
    _req: HttpRequest,
  ) -> id_effect::Effect<StreamingHttpResponse, HttpError> {
    id_effect::succeed(StreamingHttpResponse {
      status: 200,
      headers: vec![],
      body: HttpBody::empty(),
    })
  }
}

fn fixture_config() -> AiConfig {
  AiConfig::default().with_cursor_api_key("cursor-test")
}

fn agents(client: MockHttpClient) -> HttpCursorAgentsClient {
  HttpCursorAgentsClient::new(Arc::new(client), fixture_config()).expect("client")
}

async fn run<A: Send + 'static>(eff: id_effect::Effect<A, AiError>) -> Result<A, AiError> {
  TokioRuntime::current()
    .expect("tokio")
    .run(eff)
    .await
    .into_result()
    .map_err(|c| c.fail_option().expect("typed failure"))
}

#[tokio::test]
async fn cursor_list_models_fixture() {
  let models = run(agents(MockHttpClient::default()).list_models())
    .await
    .expect("models");
  assert_eq!(models[0].id, "composer-2");
  assert_eq!(models[0].display_name.as_deref(), Some("Composer 2"));
}

#[tokio::test]
async fn cursor_create_and_poll_fixture() {
  let agents = agents(MockHttpClient::default());
  let (agent, run_) = run(agents.create_agent(CreateAgentRequest {
    prompt_text: "hello".into(),
    model_id: Some("composer-2".into()),
    repos: vec![CursorRepo {
      url: "https://github.com/example/repo".into(),
      starting_ref: Some("main".into()),
    }],
  }))
  .await
  .expect("create");
  assert_eq!(agent.id, "bc-test");
  assert_eq!(run_.id, "run-1");
  let done = run(agents.wait_until_idle(&agent.id, &run_.id, Duration::from_secs(2)))
    .await
    .expect("poll");
  assert!(done.status.is_terminal());
}

#[tokio::test]
async fn cursor_get_agent_and_followup_fixture() {
  let agents = agents(MockHttpClient::default());
  let agent = run(agents.get_agent("bc-test")).await.expect("agent");
  assert_eq!(agent.latest_run_id.as_deref(), Some("run-1"));
  let followup = run(agents.send_followup("bc-test", "more"))
    .await
    .expect("followup");
  assert_eq!(followup.id, "run-2");
}

#[tokio::test]
async fn cursor_wait_until_idle_times_out() {
  let agents = agents(MockHttpClient {
    run_status: "RUNNING",
    ..MockHttpClient::default()
  });
  let err = run(agents.wait_until_idle("bc-test", "run-1", Duration::from_millis(250)))
    .await
    .unwrap_err();
  assert!(matches!(err, AiError::CursorAgents(m) if m.contains("timeout")));
}

#[tokio::test]
async fn cursor_errors_are_typed() {
  let unauthorized = agents(MockHttpClient {
    status: 401,
    ..MockHttpClient::default()
  });
  assert_eq!(
    run(unauthorized.get_agent("bc-test")).await.unwrap_err(),
    AiError::Unauthorized
  );
  assert_eq!(
    run(unauthorized.list_models()).await.unwrap_err(),
    AiError::Unauthorized
  );
  let blocked = agents(MockHttpClient {
    transport_error: Some(HttpError::InvalidRequest(
      "blocked by the SSRF policy: loopback".into(),
    )),
    ..MockHttpClient::default()
  });
  assert!(matches!(
    run(blocked.get_agent("bc-test")).await.unwrap_err(),
    AiError::Blocked(_)
  ));
  let broken = agents(MockHttpClient {
    transport_error: Some(HttpError::InvalidRequest("reset".into())),
    ..MockHttpClient::default()
  });
  assert!(matches!(
    run(broken.list_models()).await.unwrap_err(),
    AiError::CursorAgents(_)
  ));
}

#[test]
fn cursor_requires_its_key() {
  let client: Arc<dyn HttpClient> = Arc::new(MockHttpClient::default());
  assert!(HttpCursorAgentsClient::new(Arc::clone(&client), AiConfig::default()).is_err());
  assert!(
    provide_cursor_agents_client::<AiError>(Arc::clone(&client), AiConfig::default()).is_err()
  );
  let _layer = provide_cursor_agents_client::<AiError>(client, fixture_config()).expect("layer");
  let _service: Option<CursorAgentsClientService> = None;
  let err = id_effect_ai::cursor::models::list_models(
    Arc::new(MockHttpClient::default()),
    &AiConfig::default(),
  )
  .block_on()
  .into_result()
  .unwrap_err();
  assert!(err.fail_option().is_some());
}
