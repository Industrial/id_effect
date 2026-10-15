//! Cursor Cloud Agents API client.

use std::sync::Arc;
use std::time::{Duration, Instant};

use id_effect::next::{Effect, HCons, HNil, Layer, fail, hlist, sleep, succeed, suspend};
use id_effect_platform::http::{HttpClient, HttpMethod, HttpRequest};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::config::AiConfig;
use crate::cursor::types::{CreateAgentRequest, CursorAgent, CursorRun};
use crate::error::AiError;
use crate::http_util::{check_status, cursor_basic_auth_header, from_result, join_url};
use crate::retry::retry_transient_ai_http;

/// Cursor Agents API failures.
pub type CursorAgentsError = AiError;

/// Injectable Cursor Agents client service.
pub type CursorAgentsClientService = Arc<dyn CursorAgentsClient>;

/// Capability: Cursor Cloud Agents orchestration.
pub trait CursorAgentsClient: Send + Sync + 'static {
  /// `GET /v1/models` — list recommended models.
  fn list_models(&self) -> Effect<Vec<crate::cursor::types::CursorModel>, AiError>;
  /// `POST /v1/agents` — create agent and initial run.
  fn create_agent(&self, req: CreateAgentRequest) -> Effect<(CursorAgent, CursorRun), AiError>;
  /// `GET /v1/agents/{id}` — fetch agent state.
  fn get_agent(&self, agent_id: &str) -> Effect<CursorAgent, AiError>;
  /// `POST /v1/agents/{id}/runs` — send follow-up prompt.
  fn send_followup(&self, agent_id: &str, prompt_text: &str) -> Effect<CursorRun, AiError>;
  /// `GET /v1/agents/{id}/runs/{run_id}` — poll run status.
  fn get_run(&self, agent_id: &str, run_id: &str) -> Effect<CursorRun, AiError>;
  /// Poll until run reaches terminal status or timeout.
  fn wait_until_idle(
    &self,
    agent_id: &str,
    run_id: &str,
    timeout: Duration,
  ) -> Effect<CursorRun, AiError>;
}

/// HTTP-backed Cursor Agents client.
#[derive(Clone)]
pub struct HttpCursorAgentsClient {
  client: Arc<dyn HttpClient>,
  config: AiConfig,
}

impl HttpCursorAgentsClient {
  /// Create from shared HTTP client and config.
  pub fn new(client: Arc<dyn HttpClient>, config: AiConfig) -> Result<Self, AiError> {
    config.require_cursor_key()?;
    Ok(Self { client, config })
  }

  fn key(&self) -> Result<String, AiError> {
    Ok(self.config.require_cursor_key()?.expose().clone())
  }

  /// Sends `req` (built lazily) and returns the body of a 2xx response.
  fn send(
    &self,
    build: impl FnOnce(&Self) -> Result<HttpRequest, AiError>,
  ) -> Effect<Vec<u8>, AiError> {
    let req = match build(self) {
      Ok(r) => r,
      Err(e) => return fail(e),
    };
    self
      .client
      .execute(req)
      .map_err(|e| AiError::CursorAgents(format!("http: {e}")))
      .flat_map(|resp| {
        from_result(Self::check_status(&resp.status, &resp.body).map(|()| resp.body))
      })
  }

  fn get_json(&self, path: &str) -> Effect<Vec<u8>, AiError> {
    let this = self.clone();
    let path = path.to_string();
    suspend(move || {
      this.send(|this| {
        let key = this.key()?;
        Ok(HttpRequest {
          method: HttpMethod::Get,
          url: join_url(&this.config.cursor_base_url, &path),
          headers: vec![cursor_basic_auth_header(&key)],
          body: None,
          timeout: None,
          max_body_bytes: None,
        })
      })
    })
  }

  fn post_json(&self, path: &str, body: serde_json::Value) -> Effect<Vec<u8>, AiError> {
    let this = self.clone();
    let path = path.to_string();
    retry_transient_ai_http(move || {
      let this = this.clone();
      let path = path.clone();
      let body = body.clone();
      suspend(move || {
        this.send(|this| {
          let key = this.key()?;
          let bytes = serde_json::to_vec(&body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
          Ok(HttpRequest {
            method: HttpMethod::Post,
            url: join_url(&this.config.cursor_base_url, &path),
            headers: vec![
              cursor_basic_auth_header(&key),
              ("Content-Type".to_string(), "application/json".to_string()),
            ],
            body: Some(bytes),
            timeout: None,
            max_body_bytes: None,
          })
        })
      })
    })
  }

  fn check_status(status: &u16, body: &[u8]) -> Result<(), AiError> {
    check_status(*status, || String::from_utf8_lossy(body).to_string())
  }

  /// Polls `get_run` every 200 ms until the run is terminal or `deadline` has passed.
  fn poll_until_idle(
    self,
    agent_id: String,
    run_id: String,
    deadline: Instant,
  ) -> Effect<CursorRun, AiError> {
    self.get_run(&agent_id, &run_id).flat_map(move |run| {
      if run.status.is_terminal() {
        return succeed(run);
      }
      if Instant::now() >= deadline {
        return fail(AiError::CursorAgents("wait_until_idle timeout".into()));
      }
      sleep(Duration::from_millis(200)).then(suspend(move || {
        self.poll_until_idle(agent_id, run_id, deadline)
      }))
    })
  }
}

fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, AiError> {
  serde_json::from_slice(bytes).map_err(|e| AiError::InvalidJson(e.to_string()))
}

#[derive(Deserialize)]
struct CreateAgentResponse {
  agent: CursorAgent,
  run: CursorRun,
}

#[derive(Deserialize)]
struct FollowupResponse {
  run: CursorRun,
}

impl CursorAgentsClient for HttpCursorAgentsClient {
  fn list_models(&self) -> Effect<Vec<crate::cursor::types::CursorModel>, AiError> {
    crate::cursor::models::list_models(Arc::clone(&self.client), &self.config)
  }

  fn create_agent(&self, req: CreateAgentRequest) -> Effect<(CursorAgent, CursorRun), AiError> {
    let mut body = json!({
      "prompt": { "text": req.prompt_text },
    });
    if let Some(model_id) = req.model_id {
      body["model"] = json!({ "id": model_id });
    }
    if !req.repos.is_empty() {
      body["repos"] = json!(req.repos);
    }
    self.post_json("v1/agents", body).flat_map(|bytes| {
      from_result(parse_json::<CreateAgentResponse>(&bytes).map(|p| (p.agent, p.run)))
    })
  }

  fn get_agent(&self, agent_id: &str) -> Effect<CursorAgent, AiError> {
    self
      .get_json(&format!("v1/agents/{agent_id}"))
      .flat_map(|bytes| from_result(parse_json::<CursorAgent>(&bytes)))
  }

  fn send_followup(&self, agent_id: &str, prompt_text: &str) -> Effect<CursorRun, AiError> {
    let body = json!({ "prompt": { "text": prompt_text } });
    self
      .post_json(&format!("v1/agents/{agent_id}/runs"), body)
      .flat_map(|bytes| from_result(parse_json::<FollowupResponse>(&bytes).map(|p| p.run)))
  }

  fn get_run(&self, agent_id: &str, run_id: &str) -> Effect<CursorRun, AiError> {
    self
      .get_json(&format!("v1/agents/{agent_id}/runs/{run_id}"))
      .flat_map(|bytes| from_result(parse_json::<CursorRun>(&bytes)))
  }

  fn wait_until_idle(
    &self,
    agent_id: &str,
    run_id: &str,
    timeout: Duration,
  ) -> Effect<CursorRun, AiError> {
    let this = self.clone();
    let agent_id = agent_id.to_string();
    let run_id = run_id.to_string();
    suspend(move || this.poll_until_idle(agent_id, run_id, Instant::now() + timeout))
  }
}

/// Register [`CursorAgentsClient`] capability: a [`Layer`] providing
/// [`CursorAgentsClientService`].
pub fn provide_cursor_agents_client<E: Send + 'static>(
  client: Arc<dyn HttpClient>,
  config: AiConfig,
) -> Result<Layer<HCons<CursorAgentsClientService, HNil>, E>, AiError> {
  let inner: CursorAgentsClientService = Arc::new(HttpCursorAgentsClient::new(client, config)?);
  Ok(Layer::succeed(hlist![inner]))
}
