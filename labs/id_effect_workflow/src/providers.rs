//! Capability providers for duroxide workflow.

#[cfg(feature = "duroxide")]
mod duroxide_provider {
  use crate::duroxide_journal::{DuroxideStepJournal, DuroxideWorkflowRuntime};
  use id_effect::{
    Cap, CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode,
  };
  use sqlx::PgPool;
  use std::sync::Arc;

  /// Shared duroxide step journal on PostgreSQL.
  pub type DuroxideProvider = Arc<DuroxideStepJournal>;

  /// duroxide runtime configuration handle.
  pub type WorkflowRuntime = Arc<DuroxideWorkflowRuntime>;

  /// Register step journal + runtime metadata providers.
  pub fn provide_duroxide_pg(pool: PgPool, database_url: impl Into<String>) -> ProviderBox {
    struct Node {
      journal: DuroxideProvider,
      runtime: WorkflowRuntime,
    }

    impl ProviderNode for Node {
      fn id(&self) -> &str {
        "workflow/duroxide-pg"
      }

      fn requires(&self) -> &[CapabilityId] {
        &[]
      }

      fn provides(&self) -> CapabilityId {
        Cap::<DuroxideProvider>::id()
      }

      fn cap_name(&self) -> &str {
        "DuroxideProvider"
      }

      fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
        let mut out = deps.clone();
        out.insert::<Cap<DuroxideProvider>>(Arc::clone(&self.journal));
        out.insert::<Cap<WorkflowRuntime>>(Arc::clone(&self.runtime));
        Ok(out)
      }
    }

    ProviderBox(Arc::new(Node {
      journal: Arc::new(DuroxideStepJournal::new(pool)),
      runtime: Arc::new(DuroxideWorkflowRuntime::new(database_url)),
    }))
  }
}

#[cfg(feature = "duroxide")]
pub use duroxide_provider::{DuroxideProvider, WorkflowRuntime, provide_duroxide_pg};
