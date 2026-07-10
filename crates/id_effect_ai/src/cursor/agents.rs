//! Cursor Cloud Agents API client.

use std::sync::Arc;
use std::time::Duration;

use id_effect::kernel::Effect;
use id_effect::{Cap, CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode};
use id_effect_platform::http::{HttpClient, HttpMethod, HttpRequest};
use serde::Deserialize;
use serde_json::json;

use crate::config::AiConfig;
use crate::cursor::types::{CreateAgentRequest, CursorAgent, CursorRun};
use crate::error::AiError;
use crate::http_util::{cursor_basic_auth_header, join_url};
use crate::retry::retry_transient_ai_http;

/// Cursor Agents API failures.
pub type CursorAgentsError = AiError;

/// Injectable Cursor Agents client service.
pub type CursorAgentsClientService = Arc<dyn CursorAgentsClient>;

/// Capability: Cursor Cloud Agents orchestration.
pub trait CursorAgentsClient: Send + Sync + 'static {
  /// `GET /v1/models` — list recommended models.
  fn list_models(&self) -> Effect<Vec<crate::cursor::types::CursorModel>, AiError, ()>;
  /// `POST /v1/agents` — create agent and initial run.
  fn create_agent(&self, req: CreateAgentRequest) -> Effect<(CursorAgent, CursorRun), AiError, ()>;
  /// `GET /v1/agents/{id}` — fetch agent state.
  fn get_agent(&self, agent_id: &str) -> Effect<CursorAgent, AiError, ()>;
  /// `POST /v1/agents/{id}/runs` — send follow-up prompt.
  fn send_followup(&self, agent_id: &str, prompt_text: &str) -> Effect<CursorRun, AiError, ()>;
  /// `GET /v1/agents/{id}/runs/{run_id}` — poll run status.
  fn get_run(&self, agent_id: &str, run_id: &str) -> Effect<CursorRun, AiError, ()>;
  /// Poll until run reaches terminal status or timeout.
  fn wait_until_idle(
    &self,
    agent_id: &str,
    run_id: &str,
    timeout: Duration,
  ) -> Effect<CursorRun, AiError, ()>;
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

  fn get_json(&self, path: &str) -> Effect<Vec<u8>, AiError, ()> {
    let this = self.clone();
    let path = path.to_string();
    Effect::new_async(move |_r| {
      Box::pin(async move {
        let key = this.key()?;
        let req = HttpRequest {
          method: HttpMethod::Get,
          url: join_url(&this.config.cursor_base_url, &path),
          headers: vec![cursor_basic_auth_header(&key)],
          body: None,
          timeout: None,
          max_body_bytes: None,
        };
        let resp = this
          .client
          .execute(req)
          .run(&mut ())
          .await
          .map_err(|e| AiError::CursorAgents(format!("http: {e}")))?;
        Self::check_status(&resp.status, &resp.body)?;
        Ok(resp.body)
      })
    })
  }

  fn post_json(&self, path: &str, body: serde_json::Value) -> Effect<Vec<u8>, AiError, ()> {
    let this = self.clone();
    let path = path.to_string();
    retry_transient_ai_http(move || {
      let this = this.clone();
      let path = path.clone();
      let body = body.clone();
      Effect::new_async(move |_r| {
        Box::pin(async move {
          let key = this.key()?;
          let bytes = serde_json::to_vec(&body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
          let req = HttpRequest {
            method: HttpMethod::Post,
            url: join_url(&this.config.cursor_base_url, &path),
            headers: vec![
              cursor_basic_auth_header(&key),
              ("Content-Type".to_string(), "application/json".to_string()),
            ],
            body: Some(bytes),
            timeout: None,
            max_body_bytes: None,
          };
          let resp = this
            .client
            .execute(req)
            .run(&mut ())
            .await
            .map_err(|e| AiError::CursorAgents(format!("http: {e}")))?;
          Self::check_status(&resp.status, &resp.body)?;
          Ok(resp.body)
        })
      })
    })
  }

  fn check_status(status: &u16, body: &[u8]) -> Result<(), AiError> {
    if *status == 401 || *status == 403 {
      return Err(AiError::Unauthorized);
    }
    if !(200..300).contains(status) {
      return Err(AiError::from_http_status(
        *status,
        String::from_utf8_lossy(body).to_string(),
      ));
    }
    Ok(())
  }
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
  fn list_models(&self) -> Effect<Vec<crate::cursor::types::CursorModel>, AiError, ()> {
    crate::cursor::models::list_models(Arc::clone(&self.client), &self.config)
  }

  fn create_agent(&self, req: CreateAgentRequest) -> Effect<(CursorAgent, CursorRun), AiError, ()> {
    let mut body = json!({
      "prompt": { "text": req.prompt_text },
    });
    if let Some(model_id) = req.model_id {
      body["model"] = json!({ "id": model_id });
    }
    if !req.repos.is_empty() {
      body["repos"] = json!(req.repos);
    }
    let this = self.clone();
    Effect::new_async(move |_r| {
      Box::pin(async move {
        let bytes = this.post_json("v1/agents", body).run(&mut ()).await?;
        let parsed: CreateAgentResponse =
          serde_json::from_slice(&bytes).map_err(|e| AiError::InvalidJson(e.to_string()))?;
        Ok((parsed.agent, parsed.run))
      })
    })
  }

  fn get_agent(&self, agent_id: &str) -> Effect<CursorAgent, AiError, ()> {
    let this = self.clone();
    let id = agent_id.to_string();
    Effect::new_async(move |_r| {
      Box::pin(async move {
        let bytes = this
          .get_json(&format!("v1/agents/{id}"))
          .run(&mut ())
          .await?;
        let parsed: CursorAgent =
          serde_json::from_slice(&bytes).map_err(|e| AiError::InvalidJson(e.to_string()))?;
        Ok(parsed)
      })
    })
  }

  fn send_followup(&self, agent_id: &str, prompt_text: &str) -> Effect<CursorRun, AiError, ()> {
    let this = self.clone();
    let id = agent_id.to_string();
    let text = prompt_text.to_string();
    Effect::new_async(move |_r| {
      Box::pin(async move {
        let body = json!({ "prompt": { "text": text } });
        let bytes = this
          .post_json(&format!("v1/agents/{id}/runs"), body)
          .run(&mut ())
          .await?;
        let parsed: FollowupResponse =
          serde_json::from_slice(&bytes).map_err(|e| AiError::InvalidJson(e.to_string()))?;
        Ok(parsed.run)
      })
    })
  }

  fn get_run(&self, agent_id: &str, run_id: &str) -> Effect<CursorRun, AiError, ()> {
    let this = self.clone();
    let agent_id = agent_id.to_string();
    let run_id = run_id.to_string();
    Effect::new_async(move |_r| {
      Box::pin(async move {
        let bytes = this
          .get_json(&format!("v1/agents/{agent_id}/runs/{run_id}"))
          .run(&mut ())
          .await?;
        let parsed: CursorRun =
          serde_json::from_slice(&bytes).map_err(|e| AiError::InvalidJson(e.to_string()))?;
        Ok(parsed)
      })
    })
  }

  fn wait_until_idle(
    &self,
    agent_id: &str,
    run_id: &str,
    timeout: Duration,
  ) -> Effect<CursorRun, AiError, ()> {
    let this = self.clone();
    let agent_id = agent_id.to_string();
    let run_id = run_id.to_string();
    Effect::new_async(move |_r| {
      Box::pin(async move {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
          let run = this.get_run(&agent_id, &run_id).run(&mut ()).await?;
          if run.status.is_terminal() {
            return Ok(run);
          }
          if tokio::time::Instant::now() >= deadline {
            return Err(AiError::CursorAgents("wait_until_idle timeout".into()));
          }
          tokio::time::sleep(Duration::from_millis(200)).await;
        }
      })
    })
  }
}

/// Register [`CursorAgentsClient`] capability.
pub fn provide_cursor_agents_client(
  client: Arc<dyn HttpClient>,
  config: AiConfig,
) -> Result<ProviderBox, AiError> {
  let inner = Arc::new(HttpCursorAgentsClient::new(client, config)?) as Arc<dyn CursorAgentsClient>;
  struct Node {
    client: Arc<dyn CursorAgentsClient>,
  }
  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "ai/cursor-agents"
    }
    fn requires(&self) -> &[CapabilityId] {
      &[]
    }
    fn provides(&self) -> CapabilityId {
      Cap::<CursorAgentsClientService>::id()
    }
    fn cap_name(&self) -> &str {
      "CursorAgentsClient"
    }
    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let mut out = deps.clone();
      out.insert::<Cap<CursorAgentsClientService>>(Arc::clone(&self.client));
      Ok(out)
    }
  }
  Ok(ProviderBox(Arc::new(Node { client: inner })))
}
