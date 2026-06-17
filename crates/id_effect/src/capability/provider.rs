//! [`Provider`] — construct and register capabilities.

use super::env::Env;
use super::error::ProviderError;
use super::id::CapabilityId;
use super::key::CapabilityKey;
use std::sync::Arc;

/// Describes a provider node in the capability graph.
pub trait ProviderSpec: Sized + Send + Sync {
  /// Generated capability key.
  type Key: CapabilityKey<Value = Self::Output>;
  /// Concrete output type.
  type Output: Clone + Send + Sync + 'static;
  /// Stable provider id for graph diagnostics.
  fn provider_id() -> &'static str;
  /// Capability ids this provider requires.
  fn requires() -> &'static [CapabilityId] {
    &[]
  }
  /// Capability id this provider supplies.
  fn provides() -> CapabilityId {
    Self::Key::id()
  }
  /// Capability display name.
  fn cap_name() -> &'static str {
    Self::Key::name()
  }
  /// Build the service given already-built dependencies in `deps`.
  fn provide(deps: &Env) -> Result<Self::Output, ProviderError>;
}

/// Erased provider for heterogeneous collections passed to [`super::run_with`].
pub trait ProviderNode: Send + Sync {
  /// Provider id string.
  fn id(&self) -> &str;
  /// Required capability ids.
  fn requires(&self) -> &[CapabilityId];
  /// Provided capability id.
  fn provides(&self) -> CapabilityId;
  /// Capability name for diagnostics.
  fn cap_name(&self) -> &str;
  /// Build and insert into `env`.
  fn build(&self, env: &Env) -> Result<Env, ProviderError>;
}

/// Wrap a [`ProviderSpec`] as a [`ProviderNode`].
pub struct Provider<P>(std::marker::PhantomData<P>);

impl<P> Default for Provider<P> {
  fn default() -> Self {
    Self(std::marker::PhantomData)
  }
}

impl<P> ProviderNode for Provider<P>
where
  P: ProviderSpec,
{
  fn id(&self) -> &str {
    P::provider_id()
  }

  fn requires(&self) -> &[CapabilityId] {
    P::requires()
  }

  fn provides(&self) -> CapabilityId {
    P::provides()
  }

  fn cap_name(&self) -> &str {
    P::cap_name()
  }

  fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
    let value = P::provide(deps)?;
    let mut out = deps.clone();
    out.insert::<P::Key>(value);
    Ok(out)
  }
}

/// Boxed provider for `run_with` heterogeneous lists.
pub struct ProviderBox(pub Arc<dyn ProviderNode>);

impl ProviderBox {
  /// Wrap a [`ProviderSpec`].
  #[inline]
  pub fn new<P: ProviderSpec + 'static>() -> Self {
    Self(Arc::new(Provider::<P>::default()))
  }
}
