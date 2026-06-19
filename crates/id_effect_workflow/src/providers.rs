//! Capability providers for duroxide workflow.

#[cfg(feature = "duroxide")]
mod duroxide_provider {
  use crate::duroxide_journal::{DuroxideStepJournal, DuroxideWorkflowRuntime};
  use id_effect::{CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode};
  use sqlx::PgPool;
  use std::sync::Arc;

  mod step_journal_cap {
    use std::sync::Arc;

    use crate::duroxide_journal::DuroxideStepJournal;

    /// Shared duroxide step journal on PostgreSQL.
    #[::id_effect::capability(Arc<DuroxideStepJournal>)]
    #[allow(dead_code)]
    pub struct DuroxideJournal;
  }

  mod runtime_cap {
    use std::sync::Arc;

    use crate::duroxide_journal::DuroxideWorkflowRuntime;

    /// duroxide runtime configuration handle.
    #[::id_effect::capability(Arc<DuroxideWorkflowRuntime>)]
    #[allow(dead_code)]
    pub struct WorkflowRuntime;
  }

  pub use runtime_cap::WorkflowRuntimeKey;
  pub use step_journal_cap::DuroxideJournalKey as DuroxideProviderKey;

  /// Register step journal + runtime metadata providers.
  pub fn provide_duroxide_pg(pool: PgPool, database_url: impl Into<String>) -> ProviderBox {
    struct Node {
      journal: Arc<DuroxideStepJournal>,
      runtime: Arc<DuroxideWorkflowRuntime>,
    }

    impl ProviderNode for Node {
      fn id(&self) -> &str {
        "workflow/duroxide-pg"
      }

      fn requires(&self) -> &[CapabilityId] {
        &[]
      }

      fn provides(&self) -> CapabilityId {
        DuroxideProviderKey::id()
      }

      fn cap_name(&self) -> &str {
        "DuroxideProviderKey"
      }

      fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
        let mut out = deps.clone();
        out.insert::<DuroxideProviderKey>(Arc::clone(&self.journal));
        out.insert::<WorkflowRuntimeKey>(Arc::clone(&self.runtime));
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
pub use duroxide_provider::{DuroxideProviderKey, WorkflowRuntimeKey, provide_duroxide_pg};
