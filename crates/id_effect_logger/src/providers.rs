//! Capability DI v2 providers for [`EffectLogger`] and thread-local logger runtime.

use std::sync::Arc;

use ::id_effect::collections::hash_map;
use ::id_effect::{
  CapabilityId, CapabilityKey, Env, FiberRef, ProviderBox, ProviderError, ProviderNode,
  ProviderSpec, run_blocking,
};

use crate::{
  CompositeLogBackend, EffectLogKey, EffectLogMinLevelKey, EffectLogger, LogLevel,
  install_composite_log_backend, install_log_annotations_fiber_ref, install_log_spans_fiber_ref,
  install_min_log_level_fiber_ref,
};

/// Default [`EffectLogger`] provider (tracing-backed).
pub struct EffectLoggerLive;

impl ProviderSpec for EffectLoggerLive {
  type Key = EffectLogKey;
  type Output = EffectLogger;

  fn provider_id() -> &'static str {
    "effect-logger-live"
  }

  fn provide(_deps: &Env) -> Result<EffectLogger, ProviderError> {
    Ok(EffectLogger)
  }
}

/// Marker capability: fiber-local log annotation / span metadata installed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EffectLogMetadataKey;

impl ::id_effect::CapabilityKey for EffectLogMetadataKey {
  type Value = ();
}

/// Installs fiber-local annotation and span-stack refs used by [`crate::annotate_logs`] and
/// [`crate::with_log_span`].
pub struct LogMetadataLive;

impl ProviderSpec for LogMetadataLive {
  type Key = EffectLogMetadataKey;
  type Output = ();

  fn provider_id() -> &'static str {
    "effect-logger-metadata"
  }

  fn provide(_deps: &Env) -> Result<(), ProviderError> {
    let ann = run_blocking(
      FiberRef::make_with(
        hash_map::empty::<String, String>,
        |m| m.clone(),
        |p, _c| p.clone(),
      ),
      (),
    )
    .map_err(|e| ProviderError {
      provider: Self::provider_id(),
      message: format!("annotation FiberRef: {e:?}"),
    })?;
    let sp = run_blocking(
      FiberRef::make_with(Vec::<String>::new, |v| v.clone(), |p, _c| p.clone()),
      (),
    )
    .map_err(|e| ProviderError {
      provider: Self::provider_id(),
      message: format!("span FiberRef: {e:?}"),
    })?;
    install_log_annotations_fiber_ref(ann);
    install_log_spans_fiber_ref(sp);
    Ok(())
  }
}

/// Marker capability: composite log backend installed on this thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EffectLogCompositeKey;

impl ::id_effect::CapabilityKey for EffectLogCompositeKey {
  type Value = ();
}

/// Build a provider that registers `composite` for [`EffectLogger::log`] on this thread.
#[inline]
pub fn provide_composite_logger(composite: Arc<CompositeLogBackend>) -> ProviderBox {
  struct Node(Arc<CompositeLogBackend>);

  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "effect-logger-composite"
    }

    fn requires(&self) -> &[CapabilityId] {
      &[]
    }

    fn provides(&self) -> CapabilityId {
      EffectLogCompositeKey::id()
    }

    fn cap_name(&self) -> &str {
      "EffectLogCompositeKey"
    }

    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      install_composite_log_backend(self.0.clone());
      let mut out = deps.clone();
      out.insert::<EffectLogCompositeKey>(());
      Ok(out)
    }
  }

  ProviderBox(Arc::new(Node(composite)))
}

/// Build a provider that allocates a minimum [`LogLevel`] [`FiberRef`] and registers it for filtering.
#[inline]
pub fn provide_minimum_log_level(initial: LogLevel) -> ProviderBox {
  struct Node(LogLevel);

  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "effect-logger-min-level"
    }

    fn requires(&self) -> &[CapabilityId] {
      &[]
    }

    fn provides(&self) -> CapabilityId {
      EffectLogMinLevelKey::id()
    }

    fn cap_name(&self) -> &str {
      "EffectLogMinLevelKey"
    }

    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let initial = self.0;
      let fr = run_blocking(FiberRef::make(move || initial), ()).map_err(|e| ProviderError {
        provider: "effect-logger-min-level",
        message: format!("FiberRef::make: {e:?}"),
      })?;
      install_min_log_level_fiber_ref(fr.clone());
      let mut out = deps.clone();
      out.insert::<EffectLogMinLevelKey>(fr);
      Ok(out)
    }
  }

  ProviderBox(Arc::new(Node(initial)))
}
