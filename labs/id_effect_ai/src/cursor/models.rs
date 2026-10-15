//! List models endpoint.

use std::sync::Arc;

use id_effect::next::{Effect, fail, suspend};
use id_effect_platform::http::{HttpClient, HttpMethod, HttpRequest, HttpResponse};
use serde::Deserialize;

use crate::config::AiConfig;
use crate::cursor::types::CursorModel;
use crate::error::AiError;
use crate::http_util::{check_status, cursor_basic_auth_header, from_result, join_url};

#[derive(Deserialize)]
struct ListModelsResponse {
  items: Vec<CursorModel>,
}

/// Fetch recommended models (`GET /v1/models`).
pub fn list_models(
  client: Arc<dyn HttpClient>,
  config: &AiConfig,
) -> Effect<Vec<CursorModel>, AiError> {
  let key = match config.require_cursor_key() {
    Ok(k) => k.expose().clone(),
    Err(e) => return fail(e),
  };
  let req = HttpRequest {
    method: HttpMethod::Get,
    url: join_url(&config.cursor_base_url, "v1/models"),
    headers: vec![cursor_basic_auth_header(&key)],
    body: None,
    timeout: None,
    max_body_bytes: None,
  };
  suspend(move || {
    client
      .execute(req)
      .map_err(|e| AiError::CursorAgents(format!("http: {e}")))
      .flat_map(|resp| from_result(parse_models(resp)))
  })
}

fn parse_models(resp: HttpResponse) -> Result<Vec<CursorModel>, AiError> {
  check_status(resp.status, || "list models failed".to_string())?;
  let parsed: ListModelsResponse =
    serde_json::from_slice(&resp.body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
  Ok(parsed.items)
}
