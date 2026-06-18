//! [`Provider`] — construct and register capabilities.

use super::env::Env;
use super::error::ProviderError;
use super::id::CapabilityId;
use super::key::CapabilityKey;
use crate::kernel::Effect;
use crate::succeed;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Optional shutdown hook after [`super::run_with`] completes.
pub trait ShutdownHook: Send + Sync {
  /// Release resources (reverse build order).
  fn shutdown(&self);
}

/// Describes a provider node in the capability graph.
pub trait ProviderSpec: Sized + Send + Sync {
  /// Generated capability key.
  type Key: CapabilityKey<Value = Self::Output>;
  /// Concrete output type.
  type Output: Clone + Send + Sync + 'static;
  /// Stable provider id for graph diagnostics.
  fn provider_id() -> &'static str;
  /// Optional named variant (see ADR 0003).
  fn variant() -> Option<&'static str> {
    None
  }
  /// Capability ids this provider requires.
  /// Provider graph hook.
  fn requires() -> &'static [CapabilityId] {
    &[]
  }
  /// Capability id this provider supplies.
  /// Provider graph hook.
  fn provides() -> CapabilityId {
    Self::Key::id().with_variant(Self::variant())
  }
  /// Capability display name.
  /// Provider graph hook.
  fn cap_name() -> &'static str {
    Self::Key::name()
  }
  /// Build the service given already-built dependencies in `deps`.
  fn provide(deps: &Env) -> Result<Self::Output, ProviderError>;
  /// Effectful build path; default uses sync [`Self::provide`].
  fn provide_effect(_deps: &Env) -> Effect<Self::Output, ProviderError, Env> {
    Effect::new(move |env: &mut Env| Self::provide(env))
  }
  /// Set `true` when [`Self::provide_effect`] overrides the default sync wrapper.
  fn effectful_build() -> bool {
    false
  }
  /// Capability ids that may be absent without failing graph planning.
  fn optional_requires() -> &'static [CapabilityId] {
    &[]
  }
  /// When true, graph memoizes one instance per capability id.
  fn shared() -> bool {
    false
  }
  /// Optional refresh interval for long-lived providers.
  fn refresh_interval() -> Option<Duration> {
    None
  }
  /// Called on each refresh tick when [`Self::refresh_interval`] is set.
  fn on_refresh(_output: &mut Self::Output) {}
  /// Optional shutdown hook.
  fn on_shutdown(_output: &Self::Output) {}
}

/// Erased provider for heterogeneous collections passed to [`super::run_with`].
pub trait ProviderNode: Send + Sync {
  /// Provider id.
  fn id(&self) -> &str;
  /// Provider graph hook.
  fn requires(&self) -> &[CapabilityId];
  /// Provider graph hook.
  fn provides(&self) -> CapabilityId;
  /// Provider graph hook.
  fn cap_name(&self) -> &str;
  /// Provider graph hook.
  fn build(&self, env: &Env) -> Result<Env, ProviderError>;
  /// Provider graph hook.
  fn build_effect(&self, deps: &Env) -> Effect<Env, ProviderError, Env> {
    match self.build(deps) {
      Ok(env) => crate::succeed(env),
      Err(e) => crate::fail(e),
    }
  }
  /// Provider graph hook.
  fn uses_effectful_build(&self) -> bool {
    false
  }
  /// Provider graph hook.
  fn optional_requires(&self) -> &[CapabilityId] {
    &[]
  }
  /// Provider graph hook.
  fn shared(&self) -> bool {
    false
  }
  /// Provider graph hook.
  fn refresh_interval(&self) -> Option<Duration> {
    None
  }
  /// Provider graph hook.
  fn shutdown_hook(&self) -> Option<Arc<dyn ShutdownHook>> {
    None
  }
}

struct ShutdownFn<F: Fn() + Send + Sync>(F);

impl<F: Fn() + Send + Sync> ShutdownHook for ShutdownFn<F> {
  fn shutdown(&self) {
    (self.0)();
  }
}

/// Wrap a [`ProviderSpec`] as a [`ProviderNode`].
pub struct Provider<P: ProviderSpec> {
  output: Arc<Mutex<Option<P::Output>>>,
  shared: Arc<OnceLock<P::Output>>,
  _marker: std::marker::PhantomData<P>,
}

impl<P: ProviderSpec> Default for Provider<P> {
  fn default() -> Self {
    Self {
      output: Arc::new(Mutex::new(None)),
      shared: Arc::new(OnceLock::new()),
      _marker: std::marker::PhantomData,
    }
  }
}

impl<P> ProviderNode for Provider<P>
where
  P: ProviderSpec,
{
  fn id(&self) -> &str {
    P::provider_id()
  }

  /// Provider graph hook.
  fn requires(&self) -> &[CapabilityId] {
    P::requires()
  }

  /// Provider graph hook.
  fn provides(&self) -> CapabilityId {
    P::provides()
  }

  /// Provider graph hook.
  fn cap_name(&self) -> &str {
    P::cap_name()
  }

  /// Provider graph hook.
  fn optional_requires(&self) -> &[CapabilityId] {
    P::optional_requires()
  }

  /// Provider graph hook.
  fn shared(&self) -> bool {
    P::shared()
  }

  /// Provider graph hook.
  fn refresh_interval(&self) -> Option<Duration> {
    P::refresh_interval()
  }

  /// Provider graph hook.
  fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
    let value = if P::shared() {
      if let Some(v) = self.shared.get() {
        v.clone()
      } else {
        let built = P::provide(deps)?;
        let _ = self.shared.set(built.clone());
        built
      }
    } else {
      P::provide(deps)?
    };
    if let Ok(mut guard) = self.output.lock() {
      *guard = Some(value.clone());
    }
    let mut out = deps.clone();
    out.insert::<P::Key>(value);
    Ok(out)
  }

  /// Provider graph hook.
  fn build_effect(&self, deps: &Env) -> Effect<Env, ProviderError, Env> {
    let deps = deps.clone();
    let output = Arc::clone(&self.output);
    P::provide_effect(&deps).flat_map(move |value| {
      if let Ok(mut guard) = output.lock() {
        *guard = Some(value.clone());
      }
      let mut out = deps.clone();
      out.insert::<P::Key>(value);
      succeed(out)
    })
  }

  /// Provider graph hook.
  fn uses_effectful_build(&self) -> bool {
    P::effectful_build()
  }

  /// Provider graph hook.
  fn shutdown_hook(&self) -> Option<Arc<dyn ShutdownHook>> {
    let output = Arc::clone(&self.output);
    Some(Arc::new(ShutdownFn(move || {
      if let Ok(guard) = output.lock()
        && let Some(ref value) = *guard
      {
        P::on_shutdown(value);
      }
    })))
  }
}

/// Boxed provider for `run_with` heterogeneous lists.
pub struct ProviderBox(pub Arc<dyn ProviderNode>);

impl ProviderBox {
  #[inline]
  /// Wrap a [`ProviderSpec`].
  pub fn new<P: ProviderSpec + 'static>() -> Self {
    Self(Arc::new(Provider::<P>::default()))
  }
}
