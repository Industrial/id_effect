//! The pipeline's stages and their jobs (release spec 0.13.0, D1 stages 1–4 and 6).
//!
//! 1. **Ingest** ([`ingest`], [`upload`]): the document is stored for the caller's tenant and an
//!    [`ExtractJob`] is queued.
//! 2. **Extract** ([`ExtractJob`]): structured output into an `Extraction` row (and, when an
//!    [`Enricher`](crate::ai::Enricher) is configured, what the MCP server says about the
//!    organization); a [`ClassifyJob`] is queued.
//! 3. **Classify** ([`ClassifyJob`]): the bounded tool loop attaches labels; an [`EmbedJob`] is
//!    queued.
//! 4. **Embed** ([`EmbedJob`]): near-duplicates within the tenant are flagged and the document
//!    waits for review.
//! 6. **Publish** ([`publish()`]): see [the module](mod@publish).
//!
//! Each job names the tenant it acts for (written by the app, never by a client) and runs every
//! statement through the tenant router inside that tenant ([`Tenancy::scope`]). Jobs are
//! delivered at least once, so each stage checks the document's status first and does nothing
//! when an earlier delivery already moved it on. A stage that cannot succeed (the model's reply
//! does not decode, the loop runs out of budget, the document is gone) marks the document
//! [`DocumentStatus::Failed`] and discards its job; a database or dependency outage retries it.

pub mod publish;
pub mod status;

use std::sync::Arc;

use id_effect::{Effect, fail, succeed};
use id_effect_kit::jobs::{
  DrainReport, Job, JobError, JobFailure, JobOptions, JobQueueEnv, JobQueueExt, JobRegistry,
  UniqueKey, Worker,
};
use id_effect_kit::storage::{Bytes, ObjectKey};
use id_effect_kit::tenancy::Tenancy;

pub use publish::{publish, publish_hooks, published_message_id};
pub use status::{StatusEvent, StatusHub, watch};

use crate::App;
use crate::ai;
use crate::domain::{self, Document, DocumentStatus, Extraction, NewDocument, PipelineError};

/// The job queue every stage runs on.
pub const QUEUE: &str = "pipeline";

/// How often a stage job runs before it is dead (outages only: a stage that cannot succeed is
/// discarded at once).
pub const MAX_ATTEMPTS: u16 = 5;

macro_rules! stage_job {
  ($(#[$doc:meta])* $name:ident = $job:literal, $stage:literal) => {
    $(#[$doc])*
    #[derive(Clone, Debug, PartialEq, id_effect::Schema)]
    pub struct $name {
      /// The tenant the job acts for.
      pub tenant: String,
      /// The document.
      pub document_id: i64,
    }

    impl Job for $name {
      const NAME: &'static str = $job;
      const QUEUE: &'static str = QUEUE;

      fn max_attempts(&self) -> u16 {
        MAX_ATTEMPTS
      }

      fn unique_key(&self) -> Option<UniqueKey> {
        Some(UniqueKey::while_pending(format!(
          "{}:{}:{}",
          $stage, self.tenant, self.document_id
        )))
      }
    }
  };
}

stage_job!(
  /// Stage 2: read the document into an extraction.
  ExtractJob = "pipeline.extract", "extract"
);
stage_job!(
  /// Stage 3: attach labels with the bounded tool loop.
  ClassifyJob = "pipeline.classify", "classify"
);
stage_job!(
  /// Stage 4: flag near-duplicates within the tenant, then wait for review.
  EmbedJob = "pipeline.embed", "embed"
);

impl App {
  /// Queues `job` (outside any transaction: the document it names is already committed).
  pub fn enqueue<J: Job>(&self, job: J) -> Effect<(), PipelineError> {
    self
      .jobs
      .enqueue(job, JobOptions::new())
      .map(|_| ())
      .map_err(|e| PipelineError::Unavailable(format!("job queue: {e}")))
  }
}

/// Stage 1: stores `new` for the tenant and queues its extraction.
pub fn ingest(
  app: &Arc<App>,
  tenancy: &Tenancy,
  new: NewDocument,
) -> Effect<Document, PipelineError> {
  let (app, tenant) = (Arc::clone(app), tenancy.tenant().as_str().to_owned());
  tenancy
    .scope(domain::create_document(&app.db, tenancy, new))
    .flat_map(move |document| {
      app
        .enqueue(ExtractJob {
          tenant: tenant.clone(),
          document_id: document.id,
        })
        .map(move |()| {
          app
            .status
            .announce(&tenant, document.id, DocumentStatus::Ingested);
          document
        })
    })
}

/// The object key an upload is kept under: `uploads/<tenant>/<id>.txt`.
pub fn upload_key(tenant: &str, id: &str) -> Result<ObjectKey, PipelineError> {
  ObjectKey::new(format!("uploads/{tenant}/{id}.txt"))
    .map_err(|e| PipelineError::Invalid(format!("upload key: {e}")))
}

/// Stage 1 by upload: keeps the raw `bytes` in object storage, then ingests them as the body of
/// a document titled `title` from `source_id`. The body must be UTF-8 text of at most
/// [`domain::MAX_BODY_BYTES`].
pub fn upload(
  app: &Arc<App>,
  tenancy: &Tenancy,
  source_id: i64,
  title: String,
  bytes: Bytes,
) -> Effect<Document, PipelineError> {
  if bytes.len() > domain::MAX_BODY_BYTES {
    return fail(PipelineError::Invalid(format!(
      "body: at most {} bytes",
      domain::MAX_BODY_BYTES
    )));
  }
  let body = match std::str::from_utf8(&bytes) {
    Ok(text) => text.to_owned(),
    Err(_) => return fail(PipelineError::Invalid("body: not UTF-8 text".to_owned())),
  };
  let new = NewDocument {
    source_id,
    title,
    body,
  };
  if let Err(e) = new.validate() {
    return fail(e);
  }
  // Unique per process (the counter) and across restarts (the clock).
  let n = app
    .uploads
    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |d| d.as_nanos());
  let key = match upload_key(tenancy.tenant().as_str(), &format!("{nanos}-{n}")) {
    Ok(key) => key,
    Err(e) => return fail(e),
  };
  let (app, tenancy) = (Arc::clone(app), tenancy.clone());
  app
    .storage
    .put(&key, bytes)
    .map_err(|e| PipelineError::Unavailable(format!("storage: {e}")))
    .flat_map(move |_| ingest(&app, &tenancy, new))
}

/// What a stage did with its document.
enum Moved {
  /// It moved the document on to this status.
  To(Document, DocumentStatus),
  /// An earlier delivery already moved it on (to this status): nothing to do but make sure the
  /// next stage is queued.
  Already(DocumentStatus),
}

/// The document of a job when it is still at `at`; otherwise where it is now.
fn at_status(
  app: &Arc<App>,
  tenancy: &Tenancy,
  id: i64,
  at: DocumentStatus,
) -> Effect<Result<Document, Moved>, PipelineError> {
  domain::find_document(&app.db, tenancy, id).map(move |document| {
    if document.status == at.as_str() {
      Ok(document)
    } else {
      let now = DocumentStatus::parse(&document.status).unwrap_or(DocumentStatus::Failed);
      Err(Moved::Already(now))
    }
  })
}

fn extract_stage(app: Arc<App>, tenancy: Tenancy, id: i64) -> Effect<Moved, PipelineError> {
  at_status(&app, &tenancy, id, DocumentStatus::Ingested).flat_map(move |found| {
    let document = match found {
      Ok(document) => document,
      Err(already) => return succeed(already),
    };
    let settings = app.settings.clone();
    let (enricher, db, extractions) = (
      app.enricher.clone(),
      Arc::clone(&app.db),
      domain::repo::<Extraction>(&app.db, &tenancy),
    );
    ai::extract(Arc::clone(&app.model), &settings, &document)
      .flat_map(move |extracted| {
        let enrichment = match (&enricher, &extracted.organization) {
          // Enrichment is best effort: an unreachable server leaves it empty.
          (Some(enricher), Some(org)) => enricher.enrich(org).catch(|_| succeed(None)),
          _ => succeed(None),
        };
        enrichment.map(move |enrichment| (extracted, enrichment))
      })
      .flat_map(move |(extracted, enrichment)| {
        let row = Extraction {
          id: 0,
          document_id: document.id,
          title: extracted.title,
          summary: extracted.summary,
          language: extracted.language,
          organization: extracted.organization,
          enrichment,
          model: settings.model.clone(),
        };
        extractions
          .insert(row)
          .map_err(PipelineError::from)
          .flat_map(move |_| domain::set_status(&db, &tenancy, document, DocumentStatus::Extracted))
          .map(|document| Moved::To(document, DocumentStatus::Extracted))
      })
  })
}

fn classify_stage(app: Arc<App>, tenancy: Tenancy, id: i64) -> Effect<Moved, PipelineError> {
  at_status(&app, &tenancy, id, DocumentStatus::Extracted).flat_map(move |found| {
    let document = match found {
      Ok(document) => document,
      Err(already) => return succeed(already),
    };
    let tools = ai::classification_tools(&app.db, &tenancy);
    let db = Arc::clone(&app.db);
    ai::classify(Arc::clone(&app.model), &app.settings, tools, &document).flat_map(
      move |classification| {
        domain::attach_labels(&db, &tenancy, document.id, classification.labels)
          .flat_map(move |_| {
            domain::set_status(&db, &tenancy, document, DocumentStatus::Classified)
          })
          .map(|document| Moved::To(document, DocumentStatus::Classified))
      },
    )
  })
}

fn embed_stage(app: Arc<App>, tenancy: Tenancy, id: i64) -> Effect<Moved, PipelineError> {
  at_status(&app, &tenancy, id, DocumentStatus::Classified).flat_map(move |found| {
    let mut document = match found {
      Ok(document) => document,
      Err(already) => return succeed(already),
    };
    let db = Arc::clone(&app.db);
    ai::find_duplicate(
      Arc::clone(&app.embedder),
      &app.settings,
      &app.db,
      &tenancy,
      &document,
    )
    .flat_map(move |duplicate_of| {
      document.duplicate_of = duplicate_of;
      domain::set_status(&db, &tenancy, document, DocumentStatus::Review)
    })
    .map(|document| Moved::To(document, DocumentStatus::Review))
  })
}

/// Whether a stage failure can go away by itself (an outage), so the job is retried.
fn transient(e: &PipelineError) -> bool {
  matches!(
    e,
    PipelineError::Sql(_)
      | PipelineError::Unavailable(_)
      | PipelineError::Conflict(_)
      | PipelineError::NoTenant
  )
}

/// The job that follows a document moved to `status`, queued for `tenant`.
fn follow_up(
  app: &App,
  tenant: String,
  document_id: i64,
  status: DocumentStatus,
) -> Effect<(), PipelineError> {
  match status {
    DocumentStatus::Extracted => app.enqueue(ClassifyJob {
      tenant,
      document_id,
    }),
    DocumentStatus::Classified => app.enqueue(EmbedJob {
      tenant,
      document_id,
    }),
    _ => succeed(()),
  }
}

/// Marks `document_id` of `tenancy` failed (best effort) and announces it.
fn mark_failed(app: Arc<App>, tenancy: Tenancy, document_id: i64) -> Effect<(), PipelineError> {
  let (db, scoped) = (Arc::clone(&app.db), tenancy.clone());
  let mark = domain::find_document(&app.db, &tenancy, document_id)
    .flat_map(move |document| domain::set_status(&db, &scoped, document, DocumentStatus::Failed));
  tenancy.scope(mark).map(move |_| {
    app.status.announce(
      tenancy.tenant().as_str(),
      document_id,
      DocumentStatus::Failed,
    );
  })
}

/// Runs `stage` for `document_id` of `tenant`, announces the move and queues the next stage; a
/// failure that cannot go away marks the document failed and discards the job.
fn run_stage(
  app: Arc<App>,
  tenant: String,
  document_id: i64,
  stage: fn(Arc<App>, Tenancy, i64) -> Effect<Moved, PipelineError>,
) -> Effect<(), JobFailure> {
  let tenancy = Tenancy::new(tenant.clone());
  let (failing_app, failing_tenancy) = (Arc::clone(&app), tenancy.clone());
  tenancy
    .scope(stage(Arc::clone(&app), tenancy.clone(), document_id))
    .flat_map(move |moved| match moved {
      Moved::Already(status) => follow_up(&app, tenant, document_id, status),
      Moved::To(document, status) => {
        app.status.announce(&tenant, document.id, status);
        follow_up(&app, tenant, document.id, status)
      }
    })
    .catch(move |e: PipelineError| {
      if transient(&e) {
        return fail(JobFailure::retry(e));
      }
      mark_failed(failing_app, failing_tenancy, document_id)
        .catch(|_| succeed(()))
        .flat_map(move |()| fail(JobFailure::discard(e)))
    })
}

/// The handlers of the three stage jobs over `app`.
///
/// # Errors
///
/// Never in practice: the three job names are distinct.
pub fn registry(app: &Arc<App>) -> Result<JobRegistry<JobQueueEnv>, JobError> {
  let (extract_app, classify_app, embed_app) = (Arc::clone(app), Arc::clone(app), Arc::clone(app));
  JobRegistry::new()
    .register::<ExtractJob, JobFailure>(move |job, _ctx| {
      run_stage(
        Arc::clone(&extract_app),
        job.tenant,
        job.document_id,
        extract_stage,
      )
      .any_env()
    })?
    .register::<ClassifyJob, JobFailure>(move |job, _ctx| {
      run_stage(
        Arc::clone(&classify_app),
        job.tenant,
        job.document_id,
        classify_stage,
      )
      .any_env()
    })?
    .register::<EmbedJob, JobFailure>(move |job, _ctx| {
      run_stage(
        Arc::clone(&embed_app),
        job.tenant,
        job.document_id,
        embed_stage,
      )
      .any_env()
    })
}

/// A worker serving the stage jobs of `app` on [`QUEUE`].
///
/// # Errors
///
/// As [`registry`].
pub fn worker(app: &Arc<App>) -> Result<Worker<JobQueueEnv>, JobError> {
  Ok(Worker::new(registry(app)?).queue(QUEUE, 4))
}

/// Runs every stage job that is runnable now, until the queue is idle (tests, one-shot runs).
pub fn run_pending(app: &Arc<App>) -> Effect<DrainReport, JobError> {
  let worker = match worker(app) {
    Ok(worker) => worker,
    Err(e) => return fail(e),
  };
  worker
    .run_until_idle()
    .provide_all(id_effect::hlist![app.jobs.clone()])
}
