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

#[cfg(test)]
mod provider_tests {
  use super::*;
  use crate::Cap;
  use crate::provide;
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  struct Svc(pub u32);

  struct EffectfulSvc;
  impl ProviderSpec for EffectfulSvc {
    type Key = Cap<Svc>;
    type Output = Svc;
    fn provider_id() -> &'static str {
      "effectful"
    }
    fn effectful_build() -> bool {
      true
    }
    fn provide(_: &Env) -> Result<Svc, ProviderError> {
      Err(ProviderError {
        provider: "EffectfulSvc",
        message: "sync".into(),
      })
    }
    fn provide_effect(_: &Env) -> Effect<Svc, ProviderError, Env> {
      Effect::new(|_| Ok(Svc(7)))
    }
  }

  #[test]
  fn provider_box_builds_effectful() {
    use crate::run_blocking;
    let node = ProviderBox::new::<EffectfulSvc>();
    assert!(node.0.uses_effectful_build());
    let env = run_blocking(node.0.build_effect(&Env::new()), Env::new()).expect("effect build");
    assert_eq!(env.get::<Cap<Svc>>().0, 7);
  }

  #[test]
  fn provider_node_metadata() {
    let node = provide!(EffectfulSvc).0;
    assert_eq!(node.id(), "effectful");
    assert_eq!(node.cap_name(), Cap::<Svc>::name());
    assert_eq!(node.provides(), Cap::<Svc>::id());
    let hook = node.shutdown_hook().expect("shutdown hook");
    hook.shutdown();
  }

  #[test]
  fn provider_error_display() {
    let err = ProviderError {
      provider: "P",
      message: "m".into(),
    };
    assert!(err.to_string().contains("P"));
  }

  #[test]
  fn shared_provider_build_is_idempotent() {
    struct SharedDb;
    impl ProviderSpec for SharedDb {
      type Key = Cap<Svc>;
      type Output = Svc;
      fn provider_id() -> &'static str {
        "shared-svc"
      }
      fn shared() -> bool {
        true
      }
      fn provide(_: &Env) -> Result<Svc, ProviderError> {
        Ok(Svc(42))
      }
    }
    let node = provide!(SharedDb).0;
    let e1 = node.build(&Env::new()).expect("first");
    let e2 = node.build(&Env::new()).expect("second");
    assert_eq!(e1.get::<Cap<Svc>>().0, 42);
    assert_eq!(e2.get::<Cap<Svc>>().0, 42);
  }
  #[test]
  fn refresh_provider_hooks_and_optional_requires() {
    struct RefreshSvc;
    impl ProviderSpec for RefreshSvc {
      type Key = Cap<Svc>;
      type Output = Svc;
      fn provider_id() -> &'static str {
        "refresh-svc"
      }
      fn refresh_interval() -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(1))
      }
      fn on_refresh(output: &mut Self::Output) {
        output.0 += 1;
      }
      fn on_shutdown(_output: &Self::Output) {}
      fn optional_requires() -> &'static [CapabilityId] {
        static O: std::sync::LazyLock<Vec<CapabilityId>> =
          std::sync::LazyLock::new(|| vec![Cap::<Svc>::id()]);
        O.as_slice()
      }
      fn provide(_: &Env) -> Result<Svc, ProviderError> {
        Ok(Svc(1))
      }
    }
    let node = provide!(RefreshSvc).0;
    assert!(!node.optional_requires().is_empty());
    assert!(node.refresh_interval().is_some());
    let env = node.build(&Env::new()).expect("build");
    assert_eq!(env.get::<Cap<Svc>>().0, 1);
    let mut out = Svc(1);
    RefreshSvc::on_refresh(&mut out);
    assert_eq!(out.0, 2);
    RefreshSvc::on_shutdown(&Svc(1));
    node.shutdown_hook().expect("hook").shutdown();
  }

  #[test]
  fn provider_spec_default_hooks_are_exercised() {
    struct DefaultsOnly;
    impl ProviderSpec for DefaultsOnly {
      type Key = Cap<Svc>;
      type Output = Svc;
      fn provider_id() -> &'static str {
        "defaults-only"
      }
      fn provide(_: &Env) -> Result<Svc, ProviderError> {
        Ok(Svc(0))
      }
    }
    assert_eq!(DefaultsOnly::variant(), None);
    assert!(DefaultsOnly::requires().is_empty());
    assert!(!DefaultsOnly::effectful_build());
    assert!(!DefaultsOnly::shared());
    assert!(DefaultsOnly::refresh_interval().is_none());
    assert!(DefaultsOnly::optional_requires().is_empty());
    let mut v = Svc(0);
    DefaultsOnly::on_refresh(&mut v);
    DefaultsOnly::on_shutdown(&Svc(0));
    use crate::run_blocking;
    let value =
      run_blocking(DefaultsOnly::provide_effect(&Env::new()), Env::new()).expect("effect");
    assert_eq!(value, Svc(0));
  }

  #[test]
  fn provider_node_default_build_effect_surfaces_sync_error() {
    struct FailSync;
    impl ProviderSpec for FailSync {
      type Key = Cap<Svc>;
      type Output = Svc;
      fn provider_id() -> &'static str {
        "fail-sync"
      }
      fn provide(_: &Env) -> Result<Svc, ProviderError> {
        Err(ProviderError {
          provider: "FailSync",
          message: "no".into(),
        })
      }
    }
    let node = ProviderBox::new::<FailSync>().0;
    assert!(node.build(&Env::new()).is_err());
    use crate::run_blocking;
    assert!(run_blocking(node.build_effect(&Env::new()), Env::new()).is_err());
    assert!(!node.uses_effectful_build());
    assert!(!node.shared());
    assert!(node.refresh_interval().is_none());
    assert!(node.optional_requires().is_empty());
  }

  #[test]
  fn shared_provider_shutdown_hook_invokes_on_shutdown() {
    use std::sync::atomic::{AtomicU32, Ordering};
    static LAST: AtomicU32 = AtomicU32::new(0);
    struct SharedShutdown;
    impl ProviderSpec for SharedShutdown {
      type Key = Cap<Svc>;
      type Output = Svc;
      fn provider_id() -> &'static str {
        "shared-shutdown"
      }
      fn shared() -> bool {
        true
      }
      fn provide(_: &Env) -> Result<Svc, ProviderError> {
        Ok(Svc(42))
      }
      fn on_shutdown(value: &Self::Output) {
        LAST.store(value.0, Ordering::SeqCst);
      }
    }
    let node = ProviderBox::new::<SharedShutdown>().0;
    let env = node.build(&Env::new()).expect("build");
    assert_eq!(env.get::<Cap<Svc>>().0, 42);
    node.shutdown_hook().expect("hook").shutdown();
    assert_eq!(LAST.load(Ordering::SeqCst), 42);
  }

  #[test]
  fn sync_provider_build_effect_delegates_to_provide() {
    struct SyncSvc;
    impl ProviderSpec for SyncSvc {
      type Key = Cap<Svc>;
      type Output = Svc;
      fn provider_id() -> &'static str {
        "sync-svc"
      }
      fn provide(_: &Env) -> Result<Svc, ProviderError> {
        Ok(Svc(99))
      }
    }
    use crate::run_blocking;
    let node = ProviderBox::new::<SyncSvc>();
    assert!(!node.0.uses_effectful_build());
    let env = run_blocking(node.0.build_effect(&Env::new()), Env::new()).expect("build");
    assert_eq!(env.get::<Cap<Svc>>().0, 99);
  }
}
