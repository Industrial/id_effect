//! Capability DI v2 providers for [`EffectLogger`] and thread-local logger runtime.

#![allow(clippy::new_ret_no_self, clippy::unused_unit)]
use std::sync::Arc;

use ::id_effect::collections::hash_map;
use ::id_effect::{
  CapabilityId, CapabilityKey, Env, FiberRef, ProviderBox, ProviderError, ProviderNode,
  run_blocking,
};

use crate::{
  CompositeLogBackend, EffectLogMinLevelKey, EffectLogger, EffectLoggerKey, LogLevel,
  install_composite_log_backend, install_log_annotations_fiber_ref, install_log_spans_fiber_ref,
  install_min_log_level_fiber_ref,
};

/// Default [`EffectLogger`] provider (tracing-backed).
#[derive(::id_effect::ProviderSpecDerive)]
#[provides(EffectLoggerKey)]
pub struct EffectLoggerLive;

impl EffectLoggerLive {
  fn new() -> EffectLogger {
    EffectLogger
  }
}

/// Marker capability: fiber-local log annotation / span metadata installed.
#[::id_effect::capability(())]
#[allow(dead_code)]
pub struct EffectLogMetadata;

/// Installs fiber-local annotation and span-stack refs used by [`crate::annotate_logs`] and
/// [`crate::with_log_span`].
#[derive(::id_effect::ProviderSpecDerive)]
#[provides(EffectLogMetadataKey)]
pub struct LogMetadataLive;

impl LogMetadataLive {
  fn new() -> () {
    let ann = run_blocking(
      FiberRef::make_with(
        hash_map::empty::<String, String>,
        |m| m.clone(),
        |p, _c| p.clone(),
      ),
      (),
    )
    .expect("annotation FiberRef");
    let sp = run_blocking(
      FiberRef::make_with(Vec::<String>::new, |v| v.clone(), |p, _c| p.clone()),
      (),
    )
    .expect("span FiberRef");
    install_log_annotations_fiber_ref(ann);
    install_log_spans_fiber_ref(sp);
  }
}

/// Marker capability: composite log backend installed on this thread.
#[::id_effect::capability(())]
#[allow(dead_code)]
pub struct EffectLogComposite;

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
