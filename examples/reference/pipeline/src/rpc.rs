//! The Pipeline over RPC (ADR 0014) under `/rpc`: [`Intake`], a `#[rpc::service]`.
//!
//! - `ingest` (mutation, editors): stage 1 by RPC.
//! - `documents` (query): one keyset page of the tenant's documents.
//! - `document` (query): one document with its source, labels and latest extraction, read with
//!   eager loading.
//! - `publish` (mutation, reviewers): stage 6; the outbox event commits with it.
//! - `status` (subscription): a document's status now, then every change until it is final.
//!
//! Every procedure is guarded (no public procedure): the guard needs the role and a tenant claim,
//! and the procedure acts for the tenant the `TenancyLayer` resolved from the same principal
//! ([`call_tenancy`](crate::tenancy::call_tenancy)), through the tenant router. Another tenant's
//! document is `NotFound`, exactly like a missing one.

use std::sync::Arc;

use id_effect::{Ctx, DynEnv, Effect};
use id_effect_kit::rpc;
use id_effect_kit::rpc::RpcServer;
use id_effect_kit::rpc::contract::Contract;
use id_effect_kit::rpc::kinds::Subscription;
use id_effect_kit::tenancy::{Tenancy, TenantId};

use crate::App;
use crate::domain::{
  self, DocumentDetail, DocumentPage, DocumentSummary, NewDocument, PageRequest, PipelineError,
  StatusUpdate,
};
use crate::stages;
use crate::tenancy::{ingest_guard, publish_guard, read_guard, rpc_principal};

/// Where the RPC routes are mounted.
pub const RPC_PATH: &str = "/rpc";

/// The typed errors of every [`Intake`] procedure.
#[derive(Debug, PartialEq, rpc::RpcErrorUnion)]
pub enum IntakeError {
  /// The caller acts for no tenant the Pipeline serves (`403`).
  #[rpc(status = 403)]
  Forbidden {
    /// Why.
    detail: String,
  },
  /// No such document in the caller's tenant (`404`).
  #[rpc(status = 404)]
  NotFound {
    /// What was not found.
    detail: String,
  },
  /// The call conflicts with the document's state (`409`).
  #[rpc(status = 409)]
  Conflict {
    /// Why.
    detail: String,
  },
  /// The input is not acceptable (`422`).
  #[rpc(status = 422)]
  Invalid {
    /// Why.
    detail: String,
  },
  /// A dependency is away; retry later (`503`).
  #[rpc(status = 503)]
  Unavailable {
    /// Which.
    detail: String,
  },
  /// A server fault: answered as `InternalError { id }`, never sent.
  #[rpc(internal)]
  Internal {
    /// What failed (logged with the correlation id).
    detail: String,
  },
}

impl From<PipelineError> for IntakeError {
  fn from(e: PipelineError) -> Self {
    let detail = e.to_string();
    match e {
      PipelineError::NoTenant => Self::Forbidden { detail },
      PipelineError::NotFound(_) => Self::NotFound { detail },
      PipelineError::Conflict(_) => Self::Conflict { detail },
      PipelineError::Invalid(_) => Self::Invalid { detail },
      PipelineError::Unavailable(_) => Self::Unavailable { detail },
      PipelineError::Sql(_) | PipelineError::Ai(_) => Self::Internal { detail },
    }
  }
}

/// The Pipeline's intake and review surface, version 1 of its RPC contract.
#[rpc::service(version = 1)]
pub trait Intake {
  /// Ingests a document for the caller's tenant and queues its extraction.
  #[mutation]
  #[guard(ingest_guard())]
  fn ingest(&self, document: NewDocument) -> Effect<DocumentSummary, IntakeError, DynEnv>;

  /// One page of the tenant's documents, oldest first.
  #[query]
  #[guard(read_guard())]
  fn documents(&self, page: PageRequest) -> Effect<DocumentPage, IntakeError, DynEnv>;

  /// One document of the tenant with its source, labels and latest extraction.
  #[query]
  #[guard(read_guard())]
  fn document(&self, id: i64) -> Effect<DocumentDetail, IntakeError, DynEnv>;

  /// Publishes a reviewed document; `DocumentPublished` is written to the outbox with it.
  #[mutation]
  #[guard(publish_guard())]
  fn publish(&self, id: i64) -> Effect<DocumentSummary, IntakeError, DynEnv>;

  /// A document's status now, then every change until it is published or failed.
  #[subscription]
  #[guard(read_guard())]
  fn status(&self, id: i64)
  -> Effect<Subscription<StatusUpdate, IntakeError>, IntakeError, DynEnv>;
}

/// Runs `f` for the call's tenant, inside it (statements go to its database).
fn for_tenant<A: Send + 'static>(
  f: impl FnOnce(Tenancy) -> Effect<A, PipelineError> + Send + 'static,
) -> Effect<A, IntakeError, DynEnv> {
  Effect::from_ctx(move |ctx: Ctx<DynEnv>| async move {
    let tenant = ctx
      .env()
      .try_get::<TenantId>()
      .map_err(|_| IntakeError::from(PipelineError::NoTenant))?;
    let tenancy = Tenancy::new((*tenant).clone());
    ctx
      .bind(tenancy.scope(f(tenancy.clone())))
      .await
      .map_err(IntakeError::from)
  })
}

/// The handlers of [`Intake`].
#[derive(Clone)]
pub struct Procedures {
  app: Arc<App>,
}

impl Procedures {
  /// The procedures over `app`.
  pub fn new(app: Arc<App>) -> Self {
    Self { app }
  }
}

impl Intake for Procedures {
  fn ingest(&self, document: NewDocument) -> Effect<DocumentSummary, IntakeError, DynEnv> {
    let app = Arc::clone(&self.app);
    for_tenant(move |tenancy| {
      stages::ingest(&app, &tenancy, document).map(|d| DocumentSummary::from(&d))
    })
  }

  fn documents(&self, page: PageRequest) -> Effect<DocumentPage, IntakeError, DynEnv> {
    let app = Arc::clone(&self.app);
    for_tenant(move |tenancy| domain::list_documents(&app.db, &tenancy, &page))
  }

  fn document(&self, id: i64) -> Effect<DocumentDetail, IntakeError, DynEnv> {
    let app = Arc::clone(&self.app);
    for_tenant(move |tenancy| domain::document_detail(&app.db, &tenancy, id))
  }

  fn publish(&self, id: i64) -> Effect<DocumentSummary, IntakeError, DynEnv> {
    let app = Arc::clone(&self.app);
    for_tenant(move |tenancy| {
      stages::publish(&app, &tenancy, id, None).map(|d| DocumentSummary::from(&d))
    })
  }

  fn status(
    &self,
    id: i64,
  ) -> Effect<Subscription<StatusUpdate, IntakeError>, IntakeError, DynEnv> {
    let app = Arc::clone(&self.app);
    crate::tenancy::call_tenancy::<IntakeError>().flat_map(move |tenancy| {
      stages::watch::<IntakeError>(&app.db, &app.status, &tenancy, id)
        .map(Subscription::from_stream)
    })
  }
}

/// The RPC contract of the Pipeline (`rpc.contract.json` once the back office checks it in).
pub fn contract() -> Contract {
  Contract::new().with_service(IntakeService::contract())
}

/// [`Intake`] over `app`, scoping idempotency keys by `<tenant>/<principal>`.
pub fn server(app: Arc<App>) -> RpcServer {
  IntakeService::server(Procedures::new(app)).with_principal(rpc_principal)
}
