//! The data model (release spec 0.13.0, D1) and its tenant-scoped reads and writes.
//!
//! `Source` 1:N [`Document`] N:M [`Label`] (through `document_labels`), [`Document`] 1:N
//! [`Extraction`]. Every entity is tenant-scoped (`#[entity(tenant = "tenant")]`): a repository
//! reads and writes one tenant only, and the relations re-apply the child's tenant filter, so a
//! foreign key or a join-table row pointing at another tenant's row loads nothing (ADR 0013 R1).
//!
//! Reads go through [`Repository::with`](id_effect_kit::sql::Repository::with) (one `IN` query per relation, never one per row) and
//! keyset pages ([`Keyset`], cursors that stay stable while documents arrive). Every function
//! here takes the executor it runs on, so the stages pass the tenant router and the tests a
//! query-counting probe.

use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, fail, succeed};
use id_effect_kit::sql::migrate::Migrator;
use id_effect_kit::sql::query_builder::Dialect;
use id_effect_kit::sql::query_builder::sea_query::{Expr, ExprTrait, Func};
use id_effect_kit::sql::repository::{Loaded, PageCursor, SortOrder};
use id_effect_kit::sql::{
  BoundRepository, Entity, FromRow, Keyset, Paged, SqlClient, SqlError, params,
};
use id_effect_kit::tenancy::Tenancy;

/// The SQL dialect of every database the Pipeline uses.
pub const DIALECT: Dialect = Dialect::Postgres;

/// The default keyset page size of [`list_documents`].
pub const DEFAULT_PAGE_SIZE: u32 = 20;

/// The largest page [`list_documents`] returns (a larger request is capped).
pub const MAX_PAGE_SIZE: u32 = 100;

/// The longest document body accepted (bytes), by RPC and by upload.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// The most results [`search_documents`] returns.
pub const MAX_SEARCH_RESULTS: u32 = 50;

// ---- entities ----

/// Where documents come from (an inbox, a feed, a scanner).
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[from_row(crate = id_effect_kit::sql)]
#[entity(crate = id_effect_kit::sql, table = "sources", tenant = "tenant", label = "pipeline.source")]
pub struct Source {
  /// The primary key.
  #[entity(id, generated)]
  pub id: i64,
  /// A unique name within the tenant.
  #[entity(editable)]
  pub name: String,
  /// Where the source lives.
  #[entity(editable)]
  pub url: String,
  /// `unverified` or `verified`.
  #[entity(editable)]
  pub trust: String,
}

/// A label documents are classified with.
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[from_row(crate = id_effect_kit::sql)]
#[entity(crate = id_effect_kit::sql, table = "labels", tenant = "tenant", label = "pipeline.label")]
pub struct Label {
  /// The primary key.
  #[entity(id, generated)]
  pub id: i64,
  /// A unique name within the tenant (what the classifier answers with).
  #[entity(editable)]
  pub name: String,
  /// What the label means (shown to the classifier).
  #[entity(editable)]
  pub description: String,
}

/// One document moving through the pipeline.
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[from_row(crate = id_effect_kit::sql)]
#[entity(
  crate = id_effect_kit::sql,
  table = "documents",
  tenant = "tenant",
  soft_delete = "deleted_at",
  label = "pipeline.document"
)]
#[entity(belongs_to(Source), has_many(Extraction, foreign_key = "document_id"))]
#[entity(many_to_many(Label, through = "document_labels", local_key = "document_id"))]
pub struct Document {
  /// The primary key.
  #[entity(id, generated)]
  pub id: i64,
  /// The source it came from.
  pub source_id: i64,
  /// Its title.
  #[entity(editable)]
  pub title: String,
  /// Its text (untrusted: the AI stages pass it to the model as data, never as instructions).
  pub body: String,
  /// Where it is in the pipeline ([`DocumentStatus`]).
  pub status: String,
  /// The earlier document of the tenant it nearly duplicates, if any (the embed stage).
  pub duplicate_of: Option<i64>,
  /// Optimistic-locking version.
  #[entity(version)]
  pub version: i64,
}

/// What the extraction stage read from a document (one per run).
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[from_row(crate = id_effect_kit::sql)]
#[entity(crate = id_effect_kit::sql, table = "extractions", tenant = "tenant", label = "pipeline.extraction")]
pub struct Extraction {
  /// The primary key.
  #[entity(id, generated)]
  pub id: i64,
  /// The document.
  pub document_id: i64,
  /// The title the model read.
  pub title: String,
  /// A short summary.
  pub summary: String,
  /// `en` or `de`.
  pub language: String,
  /// The organization the document is from or about, if any.
  pub organization: Option<String>,
  /// What the MCP enrichment server said about the organization (JSON text), if asked.
  pub enrichment: Option<String>,
  /// The model that read it.
  pub model: String,
}

// ---- status ----

/// Where a document is in the pipeline: the `status` column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DocumentStatus {
  /// Stored; extraction is queued.
  Ingested,
  /// The extraction stage read it; classification is queued.
  Extracted,
  /// Labels are attached; the embedding stage is queued.
  Classified,
  /// Every automatic stage ran: waiting for a reviewer.
  Review,
  /// Published ([`DocumentPublished`] is in the outbox).
  Published,
  /// A stage gave up on it (the reason is in the job's last error).
  Failed,
}

impl DocumentStatus {
  /// Every status, in pipeline order.
  pub const ALL: [Self; 6] = [
    Self::Ingested,
    Self::Extracted,
    Self::Classified,
    Self::Review,
    Self::Published,
    Self::Failed,
  ];

  /// The column value.
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Ingested => "ingested",
      Self::Extracted => "extracted",
      Self::Classified => "classified",
      Self::Review => "review",
      Self::Published => "published",
      Self::Failed => "failed",
    }
  }

  /// The status of a column value.
  pub fn parse(value: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|s| s.as_str() == value)
  }

  /// Whether no stage moves a document on from here.
  pub const fn is_final(self) -> bool {
    matches!(self, Self::Published | Self::Failed)
  }
}

impl fmt::Display for DocumentStatus {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

// ---- errors ----

/// Why a pipeline operation failed.
#[derive(Clone, Debug, PartialEq)]
pub enum PipelineError {
  /// The database refused or failed.
  Sql(SqlError),
  /// No such row in the caller's tenant.
  NotFound(String),
  /// The input is not acceptable.
  Invalid(String),
  /// The operation conflicts with the current state (a stale version, a wrong status).
  Conflict(String),
  /// The caller acts for no tenant (or one without a database).
  NoTenant,
  /// An AI stage failed (the model, the tool loop, a decode); never carries prompt text.
  Ai(String),
  /// A dependency (the job queue, the object store, the MCP server) failed.
  Unavailable(String),
}

impl fmt::Display for PipelineError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Sql(e) => write!(f, "database: {e}"),
      Self::NotFound(what) => write!(f, "not found: {what}"),
      Self::Invalid(why) => write!(f, "invalid: {why}"),
      Self::Conflict(why) => write!(f, "conflict: {why}"),
      Self::NoTenant => f.write_str("the caller acts for no tenant"),
      Self::Ai(why) => write!(f, "ai: {why}"),
      Self::Unavailable(why) => write!(f, "unavailable: {why}"),
    }
  }
}

impl std::error::Error for PipelineError {}

impl From<SqlError> for PipelineError {
  fn from(e: SqlError) -> Self {
    match e {
      SqlError::RowNotFound => Self::NotFound("row".to_owned()),
      SqlError::StaleVersion { entity, id } => {
        Self::Conflict(format!("{entity} {id} was changed by someone else"))
      }
      SqlError::NotConnected => Self::NoTenant,
      other => Self::Sql(other),
    }
  }
}

// ---- wire types (RPC and MCP) ----

/// A document to ingest (the input of `Intake.ingest`).
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct NewDocument {
  /// The source it came from (a source of the caller's tenant).
  pub source_id: i64,
  /// 1 to 200 characters.
  pub title: String,
  /// The text, at most [`MAX_BODY_BYTES`].
  pub body: String,
}

impl NewDocument {
  /// Checks the title and body limits.
  ///
  /// # Errors
  ///
  /// [`PipelineError::Invalid`] naming the field.
  pub fn validate(&self) -> Result<(), PipelineError> {
    let title = self.title.trim();
    if title.is_empty() || title.chars().count() > 200 {
      return Err(PipelineError::Invalid(
        "title: must be 1 to 200 characters".to_owned(),
      ));
    }
    if self.body.trim().is_empty() || self.body.len() > MAX_BODY_BYTES {
      return Err(PipelineError::Invalid(format!(
        "body: must be 1 to {MAX_BODY_BYTES} bytes"
      )));
    }
    Ok(())
  }
}

/// One document in a list.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct DocumentSummary {
  /// The document.
  pub id: i64,
  /// Its title.
  pub title: String,
  /// Its [`DocumentStatus`].
  pub status: String,
  /// Its source.
  pub source_id: i64,
  /// The document it nearly duplicates, if any.
  pub duplicate_of: Option<i64>,
}

impl From<&Document> for DocumentSummary {
  fn from(d: &Document) -> Self {
    Self {
      id: d.id,
      title: d.title.clone(),
      status: d.status.clone(),
      source_id: d.source_id,
      duplicate_of: d.duplicate_of,
    }
  }
}

/// What the extraction stage read, as shown to clients.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct ExtractionView {
  /// The title the model read.
  pub title: String,
  /// The summary.
  pub summary: String,
  /// `en` or `de`.
  pub language: String,
  /// The organization, if any.
  pub organization: Option<String>,
  /// The enrichment (JSON text), if any.
  pub enrichment: Option<String>,
}

/// One document with its source, labels and latest extraction.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct DocumentDetail {
  /// The document.
  pub id: i64,
  /// Its title.
  pub title: String,
  /// Its text.
  pub body: String,
  /// Its [`DocumentStatus`].
  pub status: String,
  /// The source's name.
  pub source: String,
  /// The attached labels' names, sorted.
  pub labels: Vec<String>,
  /// The latest extraction, if one ran.
  pub extraction: Option<ExtractionView>,
  /// The document it nearly duplicates, if any.
  pub duplicate_of: Option<i64>,
  /// The version to send back with a change.
  pub version: i64,
}

/// Which page of documents to read (the input of `Intake.documents`).
#[derive(Clone, Debug, Default, PartialEq, id_effect::Schema)]
pub struct PageRequest {
  /// The `next` cursor of the previous page; the first page without one.
  pub cursor: Option<String>,
  /// 1 to [`MAX_PAGE_SIZE`] (default [`DEFAULT_PAGE_SIZE`]).
  pub size: Option<i64>,
}

/// One keyset page of documents, oldest first.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct DocumentPage {
  /// The documents.
  pub items: Vec<DocumentSummary>,
  /// The cursor of the next page, if there is one.
  pub next: Option<String>,
}

/// A document's status changed (the items of the `Intake.status` subscription).
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct StatusUpdate {
  /// The document.
  pub document_id: i64,
  /// Its new [`DocumentStatus`].
  pub status: String,
}

/// The event a published document announces through the outbox (topic
/// [`DOCUMENT_PUBLISHED_TOPIC`]).
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct DocumentPublished {
  /// The tenant.
  pub tenant: String,
  /// The document.
  pub document_id: i64,
  /// Its title.
  pub title: String,
  /// Its labels' names, sorted.
  pub labels: Vec<String>,
}

/// The outbox topic of [`DocumentPublished`].
pub const DOCUMENT_PUBLISHED_TOPIC: &str = "pipeline.document-published";

// ---- migrations ----

/// The Pipeline's migrations, embedded: the data model, the job queue and the outbox. Every
/// tenant database gets all of them (the queue is used in the main database only).
///
/// # Errors
///
/// When an embedded file does not parse (a build defect).
pub fn migrator() -> Result<Migrator, id_effect_kit::sql::migrate::MigrateError> {
  Migrator::from_files([
    (
      "0001_pipeline.up.sql",
      include_str!("../migrations/0001_pipeline.up.sql"),
    ),
    (
      "0001_pipeline.down.sql",
      include_str!("../migrations/0001_pipeline.down.sql"),
    ),
    (
      "0002_jobs.up.sql",
      include_str!("../migrations/0002_jobs.up.sql"),
    ),
    (
      "0002_jobs.down.sql",
      include_str!("../migrations/0002_jobs.down.sql"),
    ),
    (
      "0003_messaging.up.sql",
      include_str!("../migrations/0003_messaging.up.sql"),
    ),
    (
      "0003_messaging.down.sql",
      include_str!("../migrations/0003_messaging.down.sql"),
    ),
  ])
}

// ---- reads and writes ----

/// `T`'s repository for `tenancy` on `db`.
pub fn repo<T: Entity>(db: &Arc<dyn SqlClient>, tenancy: &Tenancy) -> BoundRepository<T> {
  tenancy.repository::<T>(DIALECT).on(Arc::clone(db))
}

/// A source of the tenant.
pub fn create_source(
  db: &Arc<dyn SqlClient>,
  tenancy: &Tenancy,
  name: &str,
  url: &str,
) -> Effect<Source, PipelineError> {
  repo::<Source>(db, tenancy)
    .insert(Source {
      id: 0,
      name: name.to_owned(),
      url: url.to_owned(),
      trust: "verified".to_owned(),
    })
    .map_err(PipelineError::from)
}

/// A label of the tenant.
pub fn create_label(
  db: &Arc<dyn SqlClient>,
  tenancy: &Tenancy,
  name: &str,
  description: &str,
) -> Effect<Label, PipelineError> {
  repo::<Label>(db, tenancy)
    .insert(Label {
      id: 0,
      name: name.to_owned(),
      description: description.to_owned(),
    })
    .map_err(PipelineError::from)
}

/// Stores `new` as an [`DocumentStatus::Ingested`] document of the tenant; its source must be
/// one of the tenant's.
pub fn create_document(
  db: &Arc<dyn SqlClient>,
  tenancy: &Tenancy,
  new: NewDocument,
) -> Effect<Document, PipelineError> {
  if let Err(e) = new.validate() {
    return fail(e);
  }
  let documents = repo::<Document>(db, tenancy);
  repo::<Source>(db, tenancy)
    .exists(new.source_id)
    .map_err(PipelineError::from)
    .flat_map(move |known| {
      if !known {
        return fail(PipelineError::Invalid(
          "source_id: no such source".to_owned(),
        ));
      }
      documents
        .insert(Document {
          id: 0,
          source_id: new.source_id,
          title: new.title.trim().to_owned(),
          body: new.body,
          status: DocumentStatus::Ingested.as_str().to_owned(),
          duplicate_of: None,
          version: 1,
        })
        .map_err(PipelineError::from)
    })
}

/// The document `id` of the tenant.
pub fn find_document(
  db: &Arc<dyn SqlClient>,
  tenancy: &Tenancy,
  id: i64,
) -> Effect<Document, PipelineError> {
  repo::<Document>(db, tenancy)
    .find(id)
    .map_err(PipelineError::from)
    .flat_map(move |found| match found {
      Some(document) => succeed(document),
      None => fail(PipelineError::NotFound(format!("document {id}"))),
    })
}

/// `document` with its status set to `status` (optimistic: a concurrent change is a
/// [`PipelineError::Conflict`]).
pub fn set_status(
  db: &Arc<dyn SqlClient>,
  tenancy: &Tenancy,
  mut document: Document,
  status: DocumentStatus,
) -> Effect<Document, PipelineError> {
  status.as_str().clone_into(&mut document.status);
  repo::<Document>(db, tenancy)
    .update(document)
    .map_err(PipelineError::from)
}

/// The documents `ids` of the tenant with their source, labels and extractions: one query for
/// the documents and one per relation, however many documents there are.
pub fn documents_with_relations(
  db: &Arc<dyn SqlClient>,
  tenancy: &Tenancy,
  ids: Vec<i64>,
) -> Effect<Vec<Loaded<Document>>, PipelineError> {
  repo::<Document>(db, tenancy)
    .with(Document::source())
    .with(Document::labels())
    .with(Document::extractions())
    .find_by(Expr::col("id").is_in(ids))
    .map_err(PipelineError::from)
}

/// One loaded document as a [`DocumentDetail`] (its labels sorted, its latest extraction).
pub fn detail_of(loaded: &Loaded<Document>) -> DocumentDetail {
  let document = loaded.entity();
  let mut labels: Vec<String> = loaded
    .get(&Document::labels())
    .iter()
    .map(|l| l.name.clone())
    .collect();
  labels.sort();
  let extraction = loaded
    .get(&Document::extractions())
    .iter()
    .max_by_key(|e| e.id)
    .map(|e| ExtractionView {
      title: e.title.clone(),
      summary: e.summary.clone(),
      language: e.language.clone(),
      organization: e.organization.clone(),
      enrichment: e.enrichment.clone(),
    });
  DocumentDetail {
    id: document.id,
    title: document.title.clone(),
    body: document.body.clone(),
    status: document.status.clone(),
    source: loaded
      .one(&Document::source())
      .map(|s| s.name.clone())
      .unwrap_or_default(),
    labels,
    extraction,
    duplicate_of: document.duplicate_of,
    version: document.version,
  }
}

/// The document `id` of the tenant with its relations ([`DocumentDetail`]).
pub fn document_detail(
  db: &Arc<dyn SqlClient>,
  tenancy: &Tenancy,
  id: i64,
) -> Effect<DocumentDetail, PipelineError> {
  documents_with_relations(db, tenancy, vec![id]).flat_map(move |found| match found.first() {
    Some(loaded) => succeed(detail_of(loaded)),
    None => fail(PipelineError::NotFound(format!("document {id}"))),
  })
}

/// The keyset of document lists: oldest first, by id (new documents land after every page
/// already read, so paging never repeats or skips one).
pub fn document_keyset(size: u32) -> Keyset {
  Keyset::new().order_by("id", SortOrder::Asc).size(size)
}

/// One page of the tenant's documents after `request.cursor`.
pub fn list_documents(
  db: &Arc<dyn SqlClient>,
  tenancy: &Tenancy,
  request: &PageRequest,
) -> Effect<DocumentPage, PipelineError> {
  let size = match request.size {
    None => DEFAULT_PAGE_SIZE,
    Some(n) if n >= 1 => std::cmp::min(u32::try_from(n).unwrap_or(MAX_PAGE_SIZE), MAX_PAGE_SIZE),
    Some(_) => {
      return fail(PipelineError::Invalid(format!(
        "size: must be 1 to {MAX_PAGE_SIZE}"
      )));
    }
  };
  let cursor = match request.cursor.as_deref().map(PageCursor::decode) {
    None => None,
    Some(Ok(cursor)) => Some(cursor),
    Some(Err(e)) => return fail(PipelineError::Invalid(format!("cursor: {e}"))),
  };
  repo::<Document>(db, tenancy)
    .find_after(&document_keyset(size), cursor)
    .map_err(PipelineError::from)
    .map(page_of)
}

/// A keyset page as a [`DocumentPage`].
pub fn page_of(page: Paged<Document>) -> DocumentPage {
  DocumentPage {
    items: page.items.iter().map(DocumentSummary::from).collect(),
    next: page.next.map(|c| c.encode()),
  }
}

/// The tenant's documents whose title contains `query` (case-insensitive), newest first, at
/// most `limit` (capped at [`MAX_SEARCH_RESULTS`]).
pub fn search_documents(
  db: &Arc<dyn SqlClient>,
  tenancy: &Tenancy,
  query: &str,
  limit: u32,
) -> Effect<Vec<DocumentSummary>, PipelineError> {
  let limit = u64::from(limit.clamp(1, MAX_SEARCH_RESULTS));
  // `%` and `_` in the query match themselves.
  let escaped = query
    .replace('\\', "\\\\")
    .replace('%', "\\%")
    .replace('_', "\\_");
  repo::<Document>(db, tenancy)
    .find_page(
      Expr::expr(Func::lower(Expr::col("title"))).like(format!("%{}%", escaped.to_lowercase())),
      id_effect_kit::sql::Page::new(limit, 0),
    )
    .map_err(PipelineError::from)
    .map(|mut found| {
      found.reverse();
      found.iter().map(DocumentSummary::from).collect()
    })
}

/// The tenant's labels, by name.
pub fn list_labels(
  db: &Arc<dyn SqlClient>,
  tenancy: &Tenancy,
) -> Effect<Vec<Label>, PipelineError> {
  repo::<Label>(db, tenancy)
    .find_all()
    .map_err(PipelineError::from)
    .map(|mut labels| {
      labels.sort_by(|a, b| a.name.cmp(&b.name));
      labels
    })
}

const ATTACH_LABEL: &str = "INSERT INTO document_labels (document_id, label_id) \
  SELECT d.id, l.id FROM documents d, labels l \
  WHERE d.id = $1 AND d.tenant = $2 AND l.tenant = $2 AND l.name = $3 \
  ON CONFLICT DO NOTHING";

/// Attaches the tenant's labels named `names` to the tenant's document `document_id` (a name the
/// tenant has no label for, or a document of another tenant, attaches nothing). Returns how many
/// were attached.
pub fn attach_labels(
  db: &Arc<dyn SqlClient>,
  tenancy: &Tenancy,
  document_id: i64,
  names: Vec<String>,
) -> Effect<u64, PipelineError> {
  let tenant = tenancy.tenant().as_str().to_owned();
  names.into_iter().fold(succeed(0_u64), |done, name| {
    let (db, tenant) = (Arc::clone(db), tenant.clone());
    done.flat_map(move |n| {
      db.execute(ATTACH_LABEL, &params![document_id, tenant, name])
        .map(move |added| n + added)
        .map_err(PipelineError::from)
    })
  })
}
