//! Cursor Cloud Agents API types.

use serde::{Deserialize, Serialize};

/// Cursor agent status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CursorAgentStatus {
  /// Agent is active.
  Active,
  /// Agent archived or unknown.
  #[serde(other)]
  Other,
}

/// Cursor run status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CursorRunStatus {
  /// Run being created.
  Creating,
  /// Run executing.
  Running,
  /// Run finished successfully.
  Finished,
  /// Run failed.
  Error,
  /// Run cancelled.
  Cancelled,
  /// Run expired.
  Expired,
  /// Unknown status.
  #[serde(other)]
  Other,
}

impl CursorRunStatus {
  /// Whether polling can stop.
  pub fn is_terminal(&self) -> bool {
    matches!(
      self,
      Self::Finished | Self::Error | Self::Cancelled | Self::Expired
    )
  }
}

/// Agent record returned by Cursor API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorAgent {
  /// Agent id (`bc-...`).
  pub id: String,
  /// Display name.
  pub name: Option<String>,
  /// Agent status.
  pub status: Option<CursorAgentStatus>,
  /// Latest run id when present.
  #[serde(rename = "latestRunId")]
  pub latest_run_id: Option<String>,
}

/// Run record returned by Cursor API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorRun {
  /// Run id.
  pub id: String,
  /// Parent agent id.
  #[serde(rename = "agentId")]
  pub agent_id: String,
  /// Run status.
  pub status: CursorRunStatus,
}

/// Model entry from `GET /v1/models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorModel {
  /// Model id (e.g. `composer-2`).
  pub id: String,
  /// Human-readable name when present.
  #[serde(rename = "displayName")]
  pub display_name: Option<String>,
}

/// Repository binding for agent create (optional).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorRepo {
  /// Git repository URL.
  pub url: String,
  /// Starting ref (branch).
  #[serde(rename = "startingRef")]
  pub starting_ref: Option<String>,
}

/// Request to create a Cursor agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateAgentRequest {
  /// Initial prompt text.
  pub prompt_text: String,
  /// Optional explicit model id.
  pub model_id: Option<String>,
  /// Optional repositories to attach.
  pub repos: Vec<CursorRepo>,
}
