//! The Pipeline reference app: a multi-tenant document-intake pipeline and its back office.
//!
//! Documents arrive for a tenant and move through [`Stage::ALL`] in order: ingest, AI extraction
//! into a typed record, classification by a bounded tool loop, embedding-based near-duplicate
//! detection, human review in the generated admin and an htmx review queue (English or German),
//! and publication through an outbox event (release spec 0.13.0, decision D1). It is built only
//! through `id_effect_kit`.
//!
//! The name is the app's, not `id_effect_resilience::Pipeline` (a policy type): the docs always
//! say "the Pipeline reference app".
//!
//! | Module | What |
//! |---|---|
//! | [`domain`] | `Source` 1:N `Document` N:M `Label`, `Document` 1:N `Extraction`; eager loading, keyset pages |
//! | [`tenancy`] | tenants and their databases (`TenantRouter`), bearer tokens, the guards |
//! | [`stages`](mod@stages) | ingest (RPC or upload), the extract / classify / embed jobs, publish with its outbox hook, live status |
//! | [`ai`] | structured extraction, the bounded classification loop, embeddings, MCP enrichment |
//! | [`rpc`] | `Intake`, the RPC surface (mutations, queries, the status subscription) |
//! | [`mcp`] | one guarded, Origin-checked MCP endpoint per tenant |
//!
//! [`router`] puts them on one Axum router: every route declares its access (only `/health` is
//! public), bearer JWTs authenticate, and the tenant of every request comes from its principal.
//!
//! ```
//! use pipeline::{Stage, stages};
//!
//! let all = stages().block_on().into_result().unwrap();
//! assert_eq!(all.first(), Some(&Stage::Ingest));
//! assert_eq!(Stage::Review.next(), Some(Stage::Publish));
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod ai;
pub mod domain;
pub mod mcp;
pub mod rpc;
pub mod stages;
pub mod tenancy;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::Query;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use id_effect::{DynEnv, Effect, HNil, succeed};
use id_effect_kit::ai::LanguageModelService;
use id_effect_kit::ai::embeddings::EmbedderService;
use id_effect_kit::axum::route::{Endpoint, Guarded, Problem, RouteInfo, Router};
use id_effect_kit::axum::{BodyLimitLayer, RequestIdLayer, TimeoutLayer, run_with_env};
use id_effect_kit::i18n::Catalog;
use id_effect_kit::jobs::{JobQueueService, PgJobQueue};
use id_effect_kit::sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_kit::sql::{SqlClient, SqlClientService};
use id_effect_kit::storage::{MemoryStore, ObjectStorage, ObjectStore};
use id_effect_kit::tenancy::{Tenancy, TenancyLayer, TenantResolver, TenantRouter};

use crate::ai::{AiSettings, Enricher};
use crate::domain::{DocumentSummary, PipelineError};
use crate::mcp::McpSettings;
use crate::stages::StatusHub;
use crate::tenancy::{Tenants, TokenKeys, ingest_guard};

/// Compiles and runs the README snippet as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

/// One stage a document passes through, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
  /// A document arrives for a tenant (RPC mutation or upload); one job per stage follows.
  Ingest,
  /// An AI model turns the text into a typed extraction (structured output).
  Extract,
  /// A bounded tool loop attaches labels.
  Classify,
  /// Embeddings flag near-duplicates within the tenant.
  Embed,
  /// Reviewers accept or correct the document in the back office.
  Review,
  /// A lifecycle hook writes `DocumentPublished` to the outbox.
  Publish,
}

impl Stage {
  /// Every stage, in pipeline order.
  pub const ALL: [Self; 6] = [
    Self::Ingest,
    Self::Extract,
    Self::Classify,
    Self::Embed,
    Self::Review,
    Self::Publish,
  ];

  /// The stage after this one, or `None` after [`Stage::Publish`].
  pub fn next(self) -> Option<Self> {
    Self::ALL
      .iter()
      .position(|s| *s == self)
      .and_then(|i| Self::ALL.get(i + 1).copied())
  }

  /// The stage's name (`ingest`, `extract`, …).
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Ingest => "ingest",
      Self::Extract => "extract",
      Self::Classify => "classify",
      Self::Embed => "embed",
      Self::Review => "review",
      Self::Publish => "publish",
    }
  }
}

impl fmt::Display for Stage {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// The pipeline's stages, as an effect.
pub fn stages() -> Effect<Vec<Stage>> {
  succeed(Stage::ALL.to_vec())
}

// ---- the application ----

/// Everything the stages and the surfaces share.
pub struct App {
  /// The configured tenants.
  pub tenants: Tenants,
  /// Every tenant statement goes here: the [`TenantRouter`] as a client (the statement runs on
  /// the database of the tenant the fiber acts for; none outside a tenant).
  pub db: Arc<dyn SqlClient>,
  /// The tenant router itself (pool counts, explicit routing).
  pub router: TenantRouter,
  /// The main (control) database: the job queue.
  pub control: Arc<dyn SqlClient>,
  /// The job queue on the control database.
  pub jobs: JobQueueService,
  /// The chat model of the extraction and classification stages.
  pub model: LanguageModelService,
  /// The embedder of the embed stage.
  pub embedder: EmbedderService,
  /// The MCP enrichment client, if configured.
  pub enricher: Option<Enricher>,
  /// Where uploads are kept.
  pub storage: ObjectStorage,
  /// Live status announcements.
  pub status: StatusHub,
  /// How the AI stages run.
  pub settings: AiSettings,
  /// The en/de messages (extraction labels, MCP tool descriptions).
  pub messages: Catalog,
  /// Upload counter (object keys).
  pub uploads: AtomicU64,
}

impl fmt::Debug for App {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("App")
      .field("tenants", &self.tenants)
      .field("router", &self.router)
      .field("settings", &self.settings)
      .finish_non_exhaustive()
  }
}

impl App {
  /// The app over `tenants` routed by `router`, the job queue on `control`, and the AI
  /// capabilities `model` and `embedder`; uploads in memory, no enrichment, default settings.
  pub fn new(
    tenants: Tenants,
    router: TenantRouter,
    control: Arc<dyn SqlClient>,
    model: LanguageModelService,
    embedder: EmbedderService,
  ) -> Self {
    let jobs = JobQueueService::new(PgJobQueue::from_service(SqlClientService(Arc::clone(
      &control,
    ))));
    Self {
      tenants,
      db: Arc::new(router.clone()),
      router,
      control,
      jobs,
      model,
      embedder,
      enricher: None,
      storage: ObjectStorage::new(MemoryStore::new()),
      status: StatusHub::new(),
      settings: AiSettings::default(),
      messages: ai::messages(),
      uploads: AtomicU64::new(0),
    }
  }

  /// Enriches extracted organizations with `enricher`.
  #[must_use]
  pub fn with_enricher(mut self, enricher: Enricher) -> Self {
    self.enricher = Some(enricher);
    self
  }

  /// Keeps uploads in `store`.
  #[must_use]
  pub fn with_storage(mut self, store: impl ObjectStore) -> Self {
    self.storage = ObjectStorage::new(store);
    self
  }

  /// Runs the AI stages with `settings`.
  #[must_use]
  pub fn with_settings(mut self, settings: AiSettings) -> Self {
    self.settings = settings;
    self
  }
}

/// Opens a pool of `max_connections` to `url`.
///
/// # Errors
///
/// When the pool cannot be created.
pub async fn open_pool(url: &str, max_connections: u32) -> Result<Arc<dyn SqlClient>, String> {
  let pool = pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(max_connections))
    .await
    .map_err(|e| format!("database: {e:?}"))?;
  Ok(Arc::new(PgSqlClient::new(pool)))
}

/// Runs the Pipeline's migrations on every database of `tenants` (the main one at `main_url`
/// first), each under the migration lock.
///
/// # Errors
///
/// When a database cannot be reached or a migration fails.
pub async fn migrate(tenants: &Tenants, main_url: &str) -> Result<(), String> {
  let migrator = domain::migrator().map_err(|e| format!("migrations: {e}"))?;
  for url in tenants.database_urls(main_url) {
    let db = open_pool(&url, 2).await?;
    let run = migrator.run_on(Arc::clone(&db));
    run_with_env(HNil, move |_| run)
      .await
      .into_result()
      .map_err(|cause| format!("migrations: {cause:?}"))?;
    let close = db.close();
    let _ = run_with_env(HNil, move |_| close).await;
  }
  Ok(())
}

// ---- the router ----

/// Where uploads go: `POST /documents/upload?source_id=<id>&title=<title>` with the text as the
/// body.
pub const UPLOAD_PATH: &str = "/documents/upload";

/// The largest request body accepted (MCP messages are capped at 1 MiB on their own).
pub const BODY_LIMIT: usize = 1024 * 1024;

/// How long a request may take to start its response.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A Problem Details response for `e`.
fn problem(e: &PipelineError) -> Response {
  let status = match e {
    PipelineError::NoTenant => StatusCode::FORBIDDEN,
    PipelineError::NotFound(_) => StatusCode::NOT_FOUND,
    PipelineError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
    PipelineError::Conflict(_) => StatusCode::CONFLICT,
    PipelineError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
    PipelineError::Sql(_) | PipelineError::Ai(_) => StatusCode::INTERNAL_SERVER_ERROR,
  };
  let detail = if status == StatusCode::INTERNAL_SERVER_ERROR {
    "internal error".to_owned()
  } else {
    e.to_string()
  };
  Problem::new(status).with_detail(detail).into_response()
}

async fn upload_handler(
  app: Arc<App>,
  tenancy: Option<Tenancy>,
  params: HashMap<String, String>,
  body: Bytes,
) -> Response {
  let Some(tenancy) = tenancy else {
    return problem(&PipelineError::NoTenant);
  };
  let Some(source_id) = params.get("source_id").and_then(|s| s.parse::<i64>().ok()) else {
    return problem(&PipelineError::Invalid(
      "source_id: a source id is required".to_owned(),
    ));
  };
  let title = params.get("title").cloned().unwrap_or_default();
  let upload = stages::upload(&app, &tenancy, source_id, title, body);
  match run_with_env(HNil, move |_| upload).await.into_result() {
    Ok(document) => {
      let summary = DocumentSummary::from(&document);
      (
        StatusCode::CREATED,
        axum::Json(serde_json::json!({
          "id": summary.id,
          "title": summary.title,
          "status": summary.status,
          "source_id": summary.source_id,
        })),
      )
        .into_response()
    }
    Err(cause) => match cause.fail_option() {
      Some(e) => problem(&e),
      None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    },
  }
}

/// `POST /documents/upload`, behind the ingest guard.
fn upload_endpoint(app: &Arc<App>) -> Endpoint<DynEnv, Guarded> {
  let app = Arc::clone(app);
  let handler =
    move |tenancy: Option<Tenancy>, Query(params): Query<HashMap<String, String>>, body: Bytes| {
      let app = Arc::clone(&app);
      async move { upload_handler(app, tenancy, params, body).await }
    };
  Endpoint::from_parts(
    RouteInfo::new(Method::POST, UPLOAD_PATH),
    axum::routing::post(handler),
  )
  .guard(ingest_guard())
}

/// `GET /health`, the only public route.
fn health_endpoint() -> Endpoint<DynEnv, id_effect_kit::axum::route::Public> {
  Endpoint::from_parts(
    RouteInfo::new(Method::GET, "/health"),
    axum::routing::get(|| async { "ok" }),
  )
  .public()
}

/// The typed routes of `app`: `/rpc` (every procedure guarded), the upload route, one MCP
/// endpoint per tenant, and `/health`.
///
/// # Errors
///
/// When the MCP settings are invalid.
pub fn routes(app: &Arc<App>, mcp: &McpSettings) -> Result<Router<DynEnv>, String> {
  let typed = Router::new()
    .route(health_endpoint())
    .route(upload_endpoint(app))
    .merge(mcp::routes(app, mcp)?);
  Ok(rpc::server(Arc::clone(app)).mount(typed))
}

/// The application: [`routes`] behind the deny-by-default layer, the tenancy layer (the tenant of
/// each request from its principal), bearer authentication, the body limit, the request timeout
/// and request ids.
///
/// # Errors
///
/// As [`routes`].
pub fn router(app: &Arc<App>, keys: &TokenKeys, mcp: &McpSettings) -> Result<axum::Router, String> {
  let typed = routes(app, mcp)?;
  Ok(
    typed
      .with_state::<()>(DynEnv::new())
      .into_axum()
      .layer(TenancyLayer::new(&TenantResolver::new()))
      .layer(keys.layer())
      .layer(BodyLimitLayer::new(BODY_LIMIT))
      .layer(TimeoutLayer::new(REQUEST_TIMEOUT))
      .layer(RequestIdLayer::new()),
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn stages_chain_in_order_and_end_at_publish() {
    let mut stage = Stage::Ingest;
    let mut seen = vec![stage];
    while let Some(next) = stage.next() {
      seen.push(next);
      stage = next;
    }
    assert_eq!(seen, Stage::ALL);
    let names: Vec<String> = seen.iter().map(ToString::to_string).collect();
    assert_eq!(
      names,
      [
        "ingest", "extract", "classify", "embed", "review", "publish"
      ]
    );
  }

  #[test]
  fn problems_hide_internal_details() {
    let res = problem(&PipelineError::Ai("the model said something".into()));
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
      problem(&PipelineError::NoTenant).status(),
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      problem(&PipelineError::Conflict("x".into())).status(),
      StatusCode::CONFLICT
    );
  }
}
