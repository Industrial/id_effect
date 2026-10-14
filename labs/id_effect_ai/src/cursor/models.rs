//! List models endpoint.

use std::sync::Arc;

use id_effect::kernel::Effect;
use id_effect_platform::http::{HttpClient, HttpMethod, HttpRequest};
use serde::Deserialize;

use crate::config::AiConfig;
use crate::cursor::types::CursorModel;
use crate::error::AiError;
use crate::http_util::{cursor_basic_auth_header, join_url};

#[derive(Deserialize)]
struct ListModelsResponse {
  items: Vec<CursorModel>,
}

/// Fetch recommended models (`GET /v1/models`).
pub fn list_models(
  client: Arc<dyn HttpClient>,
  config: &AiConfig,
) -> Effect<Vec<CursorModel>, AiError, ()> {
  let key = match config.require_cursor_key() {
    Ok(k) => k.expose().clone(),
    Err(e) => return id_effect::fail(e),
  };
  let base = config.cursor_base_url.clone();
  Effect::new_async(move |_r| {
    Box::pin(async move {
      let req = HttpRequest {
        method: HttpMethod::Get,
        url: join_url(&base, "v1/models"),
        headers: vec![cursor_basic_auth_header(&key)],
        body: None,
        timeout: None,
        max_body_bytes: None,
      };
      let resp = client
        .execute(req)
        .run(&mut ())
        .await
        .map_err(|e| AiError::CursorAgents(format!("http: {e}")))?;
      if resp.status == 401 || resp.status == 403 {
        return Err(AiError::Unauthorized);
      }
      if !(200..300).contains(&resp.status) {
        return Err(AiError::from_http_status(
          resp.status,
          "list models failed".to_string(),
        ));
      }
      let parsed: ListModelsResponse =
        serde_json::from_slice(&resp.body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
      Ok(parsed.items)
    })
  })
}
