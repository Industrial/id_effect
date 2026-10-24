//! The Pipeline service: migrations, the stage worker and the HTTP surface on one process.
//!
//! Configuration (environment):
//!
//! | Variable | Meaning |
//! |---|---|
//! | `PIPELINE_DATABASE_URL` | the main database (jobs, and every tenant without its own) — required |
//! | `PIPELINE_TENANTS` | `id:locale` pairs, comma-separated (default `acme:en,globex:de`) |
//! | `PIPELINE_TENANT_DATABASE_<ID>` | a tenant's own database (`ID` upper-cased, `-` → `_`) |
//! | `PIPELINE_TOKEN_SECRET`, `PIPELINE_TOKEN_ISSUER` | the HS256 secret (≥ 32 bytes) and issuer of bearer tokens — required |
//! | `PIPELINE_ADDR` | where to listen (default `127.0.0.1:8080`) |
//! | `PIPELINE_AI_VENDOR` | `openai` (default) or `anthropic`; keys and base URLs as `id_effect_ai::AiConfig::from_env` reads them |
//! | `PIPELINE_MCP_ORIGINS` | browser origins admitted by the MCP endpoints, comma-separated (default none) |
//! | `PIPELINE_ENRICH_URL`, `PIPELINE_ENRICH_TOOL` | the MCP enrichment server and its tool (default none) |
//! | `PIPELINE_UPLOAD_DIR` | where uploads are kept (default: in memory) |
//! | `ID_EFFECT_PROFILE` | the security-posture profile (default `dev`); `prod` refuses to start on a failed probe |

use std::sync::Arc;

use id_effect::{HNil, hlist};
use id_effect_kit::ai::embeddings::EmbedderService;
use id_effect_kit::ai::mcp::McpClient;
use id_effect_kit::ai::{
  AiConfig, AnthropicLanguageModel, LanguageModelService, OpenAiEmbedder, OpenAiLanguageModel,
};
use id_effect_kit::axum::run_with_env;
use id_effect_kit::platform::http::HttpClientConfig;
use id_effect_kit::storage::LocalStore;
use pipeline::ai::Enricher;
use pipeline::mcp::McpSettings;
use pipeline::tenancy::{TenantConfig, Tenants, TokenKeys};
use pipeline::{App, migrate, open_pool, router, stages};

fn env(key: &str) -> Option<String> {
  std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn required(key: &str) -> Result<String, String> {
  env(key).ok_or_else(|| format!("{key} is required"))
}

fn tenants() -> Result<Tenants, String> {
  let spec = env("PIPELINE_TENANTS").unwrap_or_else(|| "acme:en,globex:de".to_owned());
  let mut list = Vec::new();
  for pair in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
    let (id, locale) = pair
      .split_once(':')
      .ok_or_else(|| format!("PIPELINE_TENANTS: {pair:?} is not id:locale"))?;
    let mut tenant = TenantConfig::new(id, locale)?;
    let key = format!(
      "PIPELINE_TENANT_DATABASE_{}",
      id.to_ascii_uppercase().replace('-', "_")
    );
    if let Some(url) = env(&key) {
      tenant = tenant.with_database(url);
    }
    list.push(tenant);
  }
  Ok(Tenants::new(list))
}

fn model(config: &AiConfig) -> Result<LanguageModelService, String> {
  let vendor = env("PIPELINE_AI_VENDOR").unwrap_or_else(|| "openai".to_owned());
  match vendor.as_str() {
    "openai" => OpenAiLanguageModel::from_config(config)
      .map(|m| Arc::new(m) as LanguageModelService)
      .map_err(|e| format!("openai: {e}")),
    "anthropic" => AnthropicLanguageModel::from_config(config)
      .map(|m| Arc::new(m) as LanguageModelService)
      .map_err(|e| format!("anthropic: {e}")),
    other => Err(format!("PIPELINE_AI_VENDOR: unknown vendor {other:?}")),
  }
}

async fn start() -> Result<(), String> {
  let main_url = required("PIPELINE_DATABASE_URL")?;
  let tenants = tenants()?;
  let keys = TokenKeys::new(
    required("PIPELINE_TOKEN_SECRET")?.into_bytes(),
    required("PIPELINE_TOKEN_ISSUER")?,
  )?;
  let mcp = McpSettings {
    allowed_origins: env("PIPELINE_MCP_ORIGINS")
      .map(|v| v.split(',').map(|o| o.trim().to_owned()).collect())
      .unwrap_or_default(),
  };
  migrate(&tenants, &main_url).await?;
  let control = open_pool(&main_url, 8).await?;
  let ai = AiConfig::from_env();
  let embedder: EmbedderService =
    Arc::new(OpenAiEmbedder::from_config(&ai).map_err(|e| format!("embeddings: {e}"))?);
  let router_db = tenants.router(&main_url, 8);
  let mut app = App::new(tenants, router_db, control, model(&ai)?, embedder);
  if let Some(dir) = env("PIPELINE_UPLOAD_DIR") {
    app = app.with_storage(LocalStore::new(&dir).map_err(|e| format!("uploads: {e}"))?);
  }
  if let Some(url) = env("PIPELINE_ENRICH_URL") {
    let tool = env("PIPELINE_ENRICH_TOOL").unwrap_or_else(|| "lookup_organization".to_owned());
    app = app.with_enricher(Enricher::new(
      McpClient::http(url, &HttpClientConfig::default()),
      tool,
    ));
  }
  let app = Arc::new(app);
  let service = router(&app, &keys, &mcp)?;
  let worker = stages::worker(&app)
    .map_err(|e| format!("worker: {e}"))?
    .run()
    .provide_all(hlist![app.jobs.clone()]);

  // The startup gate (ADR 0013 R10), once everything is assembled (so every opt-out taken is in
  // the posture registry) and before the worker runs or a socket is bound: a weakened prod
  // profile refuses to start.
  let profile = env("ID_EFFECT_PROFILE").unwrap_or_else(|| "dev".to_owned());
  let report = pipeline::ui::posture(&app, &mcp, &ai, &profile)?
    .check()
    .enforce()
    .map_err(|refused| refused.to_string())?;
  eprint!("{report}");

  tokio::spawn(async move {
    let _ = run_with_env(HNil, move |_| worker).await;
  });

  let addr = env("PIPELINE_ADDR").unwrap_or_else(|| "127.0.0.1:8080".to_owned());
  let listener = tokio::net::TcpListener::bind(&addr)
    .await
    .map_err(|e| format!("bind {addr}: {e}"))?;
  eprintln!("pipeline listening on {addr}");
  axum::serve(listener, service)
    .with_graceful_shutdown(async {
      let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .map_err(|e| format!("serve: {e}"))
}

#[tokio::main]
async fn main() {
  if let Err(e) = start().await {
    eprintln!("pipeline: {e}");
    std::process::exit(1);
  }
}
