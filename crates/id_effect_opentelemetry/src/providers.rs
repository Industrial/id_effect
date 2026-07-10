//! Capability DI provider for an installed OpenTelemetry runtime handle.

use std::sync::Arc;

use id_effect::{Cap, CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode};

use crate::starter::OtelStarterGuard;

/// OpenTelemetry runtime handle (flush/shutdown + meter access).
pub type OtelRuntime = Arc<OtelStarterGuard>;

/// Register `guard` in the capability environment for domain programs that need OTEL flush/shutdown.
#[inline]
pub fn provide_otel_runtime(guard: Arc<OtelStarterGuard>) -> ProviderBox {
  struct Node(Arc<OtelStarterGuard>);

  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "opentelemetry/runtime"
    }

    fn requires(&self) -> &[CapabilityId] {
      &[]
    }

    fn provides(&self) -> CapabilityId {
      Cap::<OtelRuntime>::id()
    }

    fn cap_name(&self) -> &str {
      "OtelRuntime"
    }

    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let mut out = deps.clone();
      out.insert::<Cap<OtelRuntime>>(Arc::clone(&self.0));
      Ok(out)
    }
  }

  ProviderBox(Arc::new(Node(guard)))
}
