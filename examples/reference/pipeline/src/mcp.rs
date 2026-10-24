//! Stage 7, agents: the Pipeline as an MCP server (ai.llm #6).
//!
//! Each tenant has its own Streamable HTTP endpoint, `/mcp/<tenant>`, serving two read-only tools
//! over that tenant's documents: `search_documents` and `get_document`, described in the tenant's
//! locale (English for `acme`, German for `globex`). Secure by default (ADR 0013, ADR 0015):
//!
//! - **Guarded.** The endpoint only mounts behind a guard (`McpEndpoint` without one does not
//!   compile); [`agent_guard`] admits a principal with the `agent` role whose tenant claim is the
//!   endpoint's tenant. Anonymous callers get `401`, other tenants' agents `403`.
//! - **Origin-checked.** A browser request whose `Origin` is not in
//!   [`McpSettings::allowed_origins`] (empty by default) is refused (DNS rebinding).
//! - **Bounded.** Messages over 1 MiB are refused before parsing; search results are capped.
//! - **No writes.** Both tools read; `dangerous_mcp_write_tools` is never taken.
//!
//! The MCP *client* half (enrichment from an outside tool server) is
//! [`Enricher`](crate::ai::Enricher).

use std::sync::Arc;

use id_effect::{DynEnv, Effect};
use id_effect_kit::ai::mcp::{McpEndpoint, McpServer};
use id_effect_kit::ai::tools::{Tool, ToolSet};
use id_effect_kit::axum::realtime::origin::OriginPolicy;
use id_effect_kit::axum::route::Router;
use id_effect_kit::i18n::Args;
use id_effect_kit::tenancy::Tenancy;

use crate::App;
use crate::domain::{self, DocumentDetail, DocumentSummary, MAX_SEARCH_RESULTS, PipelineError};
use crate::tenancy::{TenantConfig, agent_guard};

/// The default number of results of `search_documents`.
pub const DEFAULT_SEARCH_RESULTS: u32 = 10;

/// How the MCP endpoints are served.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpSettings {
  /// Browser origins admitted (`https://agents.example.com`); empty refuses every browser.
  pub allowed_origins: Vec<String>,
}

/// The path of tenant `tenant`'s MCP endpoint.
pub fn endpoint_path(tenant: &str) -> String {
  format!("/mcp/{tenant}")
}

/// The input of `search_documents`.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct SearchQuery {
  /// Text the title contains (case-insensitive).
  pub query: String,
  /// At most this many results (1 to 50, default 10).
  pub limit: Option<i64>,
}

/// The output of `search_documents`.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct SearchResults {
  /// The matching documents, oldest first.
  pub documents: Vec<DocumentSummary>,
}

/// The input of `get_document`.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct DocumentRef {
  /// The document's id.
  pub id: i64,
}

/// The tools of `tenant`'s endpoint over `app`, described in the tenant's locale.
///
/// # Panics
///
/// Never: the two tool names are valid and distinct.
pub fn tools(app: &Arc<App>, tenant: &TenantConfig) -> ToolSet {
  let tenancy = Tenancy::new(tenant.id.clone());
  let args = Args::new()
    .with("tenant", tenant.id.clone())
    .with("limit", i64::from(MAX_SEARCH_RESULTS));
  let describe = |id: &str| app.messages.format(&tenant.locale, id, &args);
  let (search_app, search_tenancy) = (Arc::clone(app), tenancy.clone());
  let search = Tool::new(
    "search_documents",
    describe("mcp-search-documents"),
    move |q: SearchQuery| -> Effect<SearchResults, PipelineError> {
      let limit = q
        .limit
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(DEFAULT_SEARCH_RESULTS);
      let read = domain::search_documents(&search_app.db, &search_tenancy, &q.query, limit)
        .map(|documents| SearchResults { documents });
      search_tenancy.scope(read)
    },
  );
  let (get_app, get_tenancy) = (Arc::clone(app), tenancy);
  let get = Tool::new(
    "get_document",
    describe("mcp-get-document"),
    move |r: DocumentRef| -> Effect<DocumentDetail, PipelineError> {
      get_tenancy.scope(domain::document_detail(&get_app.db, &get_tenancy, r.id))
    },
  );
  ToolSet::new()
    .with(search)
    .and_then(|set| set.with(get))
    .expect("two distinct, valid tool names")
}

/// The MCP server of `tenant`.
pub fn server(app: &Arc<App>, tenant: &TenantConfig) -> McpServer {
  McpServer::new(format!("pipeline-{}", tenant.id), env!("CARGO_PKG_VERSION"))
    .with_instructions("Read-only access to the documents of one tenant of the Pipeline.")
    .with_tools(tools(app, tenant))
}

/// Every tenant's guarded, Origin-checked MCP endpoint, as typed routes.
///
/// # Errors
///
/// When an allowed origin is not a valid origin.
pub fn routes(app: &Arc<App>, settings: &McpSettings) -> Result<Router<DynEnv>, String> {
  let origins = if settings.allowed_origins.is_empty() {
    OriginPolicy::deny_all()
  } else {
    OriginPolicy::allow(settings.allowed_origins.iter().cloned())
      .map_err(|e| format!("mcp allowed origins: {e}"))?
  };
  Ok(app.tenants.iter().fold(Router::new(), |router, tenant| {
    let endpoint = McpEndpoint::new(server(app, tenant))
      .at(endpoint_path(&tenant.id))
      .allow_origins(origins.clone())
      .guard(agent_guard(&tenant.id));
    router.merge(endpoint.router::<DynEnv>())
  }))
}
