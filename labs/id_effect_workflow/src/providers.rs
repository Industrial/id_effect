//! Capability providers for duroxide workflow.

#[cfg(feature = "duroxide")]
mod duroxide_provider {
  use crate::duroxide_journal::{DuroxideStepJournal, DuroxideWorkflowRuntime};
  use id_effect::next::{HCons, HNil, Layer, hlist};
  use sqlx::PgPool;
  use std::sync::Arc;

  /// Shared duroxide step journal on PostgreSQL.
  pub type DuroxideProvider = Arc<DuroxideStepJournal>;

  /// duroxide runtime configuration handle.
  pub type WorkflowRuntime = Arc<DuroxideWorkflowRuntime>;

  /// A [`Layer`] providing the step journal ([`DuroxideProvider`]) and the runtime metadata
  /// ([`WorkflowRuntime`]) in the typed environment. The layer does not close the pool (the
  /// caller owns it).
  pub fn provide_duroxide_pg<E: Send + 'static>(
    pool: PgPool,
    database_url: impl Into<String>,
  ) -> Layer<HCons<DuroxideProvider, HCons<WorkflowRuntime, HNil>>, E> {
    let journal: DuroxideProvider = Arc::new(DuroxideStepJournal::new(pool));
    let runtime: WorkflowRuntime = Arc::new(DuroxideWorkflowRuntime::new(database_url));
    Layer::succeed(hlist![journal, runtime])
  }
}

#[cfg(feature = "duroxide")]
pub use duroxide_provider::{DuroxideProvider, WorkflowRuntime, provide_duroxide_pg};
