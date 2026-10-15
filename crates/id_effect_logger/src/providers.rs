//! [`Layer`]s for [`EffectLogger`] and the thread-local logger pipeline (0.7 kernel).
//!
//! Each layer needs nothing (`In = HNil`) and never fails, so its error type `E` is whatever the
//! graph it joins uses. The pipeline layers install their thread-local state when they are
//! **built**, on the thread that drives the build (the thread running the program for
//! [`Effect::block_on`](id_effect::Effect::block_on)).

use std::collections::HashMap;
use std::sync::Arc;

use ::id_effect::{FiberRef, HCons, HNil, Layer, hlist, sync};

use crate::{
  CompositeLogBackend, EffectLogMinLevel, EffectLogger, LogLevel, install_composite_log_backend,
  install_log_annotations_fiber_ref, install_log_spans_fiber_ref, install_min_log_level_fiber_ref,
};

/// Default [`EffectLogger`] provider (tracing-backed).
#[derive(Clone, Copy, Debug, Default)]
pub struct EffectLoggerLive;

impl EffectLoggerLive {
  /// The layer: [`EffectLogger`] as a capability.
  #[inline]
  pub fn layer<E: Send + 'static>() -> Layer<HCons<EffectLogger, HNil>, E> {
    Layer::succeed(hlist![EffectLogger])
  }
}

/// Marker capability: fiber-local log annotation / span metadata installed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EffectLogMetadata;

/// Installs fiber-local annotation and span-stack refs used by [`crate::annotate_logs`] and
/// [`crate::with_log_span`].
#[derive(Clone, Copy, Debug, Default)]
pub struct LogMetadataLive;

impl LogMetadataLive {
  /// The layer: each build installs fresh annotation and span-stack [`FiberRef`]s on the
  /// building thread and provides the [`EffectLogMetadata`] marker.
  #[inline]
  pub fn layer<E: Send + 'static>() -> Layer<HCons<EffectLogMetadata, HNil>, E> {
    Layer::effect(|| {
      sync(|| {
        // A forked child starts with a copy of its parent's value (FiberRef inheritance).
        install_log_annotations_fiber_ref(FiberRef::new(HashMap::<String, String>::new()));
        install_log_spans_fiber_ref(FiberRef::new(Vec::<String>::new()));
        hlist![EffectLogMetadata]
      })
    })
  }
}

/// Marker capability: composite log backend installed on this thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EffectLogComposite;

/// A layer that registers `composite` for [`EffectLogger::log`] on the building thread.
#[inline]
pub fn provide_composite_logger<E: Send + 'static>(
  composite: Arc<CompositeLogBackend>,
) -> Layer<HCons<EffectLogComposite, HNil>, E> {
  Layer::effect(move || {
    let composite = Arc::clone(&composite);
    sync(move || {
      install_composite_log_backend(composite);
      hlist![EffectLogComposite]
    })
  })
}

/// A layer that allocates a minimum [`LogLevel`] [`FiberRef`] starting at `initial`, registers it
/// for filtering on the building thread and provides it as [`EffectLogMinLevel`].
#[inline]
pub fn provide_minimum_log_level<E: Send + 'static>(
  initial: LogLevel,
) -> Layer<HCons<EffectLogMinLevel, HNil>, E> {
  Layer::effect(move || {
    sync(move || {
      let fr = FiberRef::new(initial);
      install_min_log_level_fiber_ref(fr.clone());
      hlist![fr]
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use ::id_effect::Effect;
  use ::id_effect::Never;

  fn build<Out: Send + 'static>(eff: Effect<Out, Never>) -> Out {
    eff.block_on().into_result().expect("layer build")
  }

  #[test]
  fn provide_composite_logger_installs_capability() {
    let composite = Arc::new(CompositeLogBackend::new());
    let HCons(marker, HNil) = build(provide_composite_logger::<Never>(composite).build());
    assert_eq!(*marker, EffectLogComposite);

    let HCons(_logger, HNil) = build(EffectLoggerLive::layer::<Never>().build());

    let HCons(meta, HNil) = build(LogMetadataLive::layer::<Never>().build());
    assert_eq!(*meta, EffectLogMetadata);
  }

  #[test]
  fn provide_minimum_log_level_installs_fiber_ref() {
    let layer = provide_minimum_log_level::<Never>(LogLevel::Warn);
    let HCons(fr, HNil) = build(layer.build());
    assert_eq!(build(fr.get()), LogLevel::Warn);
  }
}
