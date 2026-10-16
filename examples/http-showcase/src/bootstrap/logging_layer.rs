//! [`logging_layer`]: structured `key=value` log lines on stderr.

use std::sync::Arc;

use id_effect::{Layer, caps};
use id_effect_logger::{
  CompositeLogBackend, EffectLogComposite, EffectLogMetadata, EffectLogger, EffectLoggerLive,
  LogMetadataLive, Logger, StructuredLogBackend, provide_composite_logger,
};

/// [`EffectLogger`] writing `key=value` lines to stderr, with log annotations
/// (`annotate_logs`). The pipeline is installed on the thread that builds the layer, so build it
/// right before logging (as [`log_startup`](super::log_startup) does).
pub fn logging_layer<E: Send + 'static>()
-> Layer<caps!(EffectLogger, EffectLogMetadata, EffectLogComposite), E> {
  let composite = CompositeLogBackend::new();
  // Adding to a fresh composite only fails on a poisoned lock, which cannot happen here.
  let _ = composite.add(Arc::new(StructuredLogBackend::new(std::io::stderr())));
  EffectLoggerLive::layer()
    .merge(LogMetadataLive::layer())
    .merge(provide_composite_logger(Arc::new(composite)))
}
