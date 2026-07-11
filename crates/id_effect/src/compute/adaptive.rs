//! Hardware-aware parallelism context driven by Compute Fabric.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

use super::fabric::ComputeFabric;
use super::telemetry::TelemetryEngine;
use crate::Parallelism;

/// Snapshot of admission budget and parallelism knobs for the current run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveContext {
  /// Admission permits available from the supervisor.
  pub admission_budget: usize,
  /// Element-count threshold before Rayon bulk paths activate.
  pub parallelism_threshold: usize,
  /// Suggested Rayon worker count from the supervisor.
  pub rayon_threads: usize,
}

impl AdaptiveContext {
  /// Default threshold when no fabric is installed (matches [`Parallelism::DEFAULT_THRESHOLD`]).
  pub const DEFAULT_THRESHOLD: usize = Parallelism::DEFAULT_THRESHOLD;

  /// Fallback context using host parallelism only.
  #[inline]
  pub fn standalone() -> Self {
    let threads = std::thread::available_parallelism()
      .map(|n| n.get())
      .unwrap_or(4)
      .max(1);
    Self {
      admission_budget: threads,
      parallelism_threshold: Self::DEFAULT_THRESHOLD,
      rayon_threads: threads,
    }
  }

  /// Build from a live [`ComputeFabric`] supervisor snapshot.
  #[inline]
  pub fn from_fabric<E: TelemetryEngine + 'static>(fabric: &ComputeFabric<E>) -> Self {
    let admission = fabric.admission();
    let budget = admission.available().max(1);
    let threads = admission.max_permits().max(1);
    let threshold = effective_threshold_for_budget(budget);
    Self {
      admission_budget: budget,
      parallelism_threshold: threshold,
      rayon_threads: threads,
    }
  }
}

/// Effective auto-parallel threshold for `policy` using the thread-local context.
#[inline]
pub fn effective_threshold(policy: Parallelism) -> usize {
  current_adaptive_context().apply_threshold(policy)
}

impl AdaptiveContext {
  /// Threshold for `policy` given this context.
  #[inline]
  pub fn apply_threshold(self, policy: Parallelism) -> usize {
    match policy {
      Parallelism::Serial => usize::MAX,
      Parallelism::ForceParallel => 0,
      Parallelism::Auto { threshold } => threshold.min(self.parallelism_threshold),
    }
  }
}

fn effective_threshold_for_budget(budget: usize) -> usize {
  let base = AdaptiveContext::DEFAULT_THRESHOLD;
  match budget {
    n if n >= 8 => (base / 4).max(1),
    n if n >= 4 => (base / 2).max(1),
    1 => base.saturating_mul(2),
    _ => base,
  }
}

static FABRIC: OnceLock<std::sync::Mutex<Option<Arc<dyn FabricSource>>>> = OnceLock::new();

thread_local! {
  static ADAPTIVE_CTX: RefCell<AdaptiveContext> = RefCell::new(AdaptiveContext::standalone());
}

fn fabric_slot() -> &'static std::sync::Mutex<Option<Arc<dyn FabricSource>>> {
  FABRIC.get_or_init(|| std::sync::Mutex::new(None))
}

/// Type-erased fabric handle for adaptive context refresh.
pub trait FabricSource: Send + Sync {
  fn adaptive_context(&self) -> AdaptiveContext;
}

impl<E: TelemetryEngine + 'static> FabricSource for ComputeFabric<E> {
  fn adaptive_context(&self) -> AdaptiveContext {
    AdaptiveContext::from_fabric(self)
  }
}

/// Install fabric for adaptive parallelism.
pub fn install_fabric<E: TelemetryEngine + 'static>(fabric: Arc<ComputeFabric<E>>) {
  let erased: Arc<dyn FabricSource> = fabric;
  *fabric_slot().lock().expect("fabric slot mutex") = Some(erased);
  refresh_adaptive_context();
}

/// Refresh thread-local context from the installed fabric (call after supervisor ticks).
#[inline]
pub fn refresh_adaptive_context() {
  let ctx = fabric_slot()
    .lock()
    .ok()
    .and_then(|guard| guard.as_ref().map(|f| f.adaptive_context()))
    .unwrap_or_else(AdaptiveContext::standalone);
  ADAPTIVE_CTX.with(|c| *c.borrow_mut() = ctx);
}

/// Current thread-local adaptive context.
#[inline]
pub fn current_adaptive_context() -> AdaptiveContext {
  ADAPTIVE_CTX.with(|c| *c.borrow())
}

/// Run `f` with a temporary adaptive context (restores prior context afterward).
#[allow(dead_code)]
pub fn with_adaptive_context<F, R>(ctx: AdaptiveContext, f: F) -> R
where
  F: FnOnce() -> R,
{
  ADAPTIVE_CTX.with(|cell| {
    let prev = *cell.borrow();
    *cell.borrow_mut() = ctx;
    let out = f();
    *cell.borrow_mut() = prev;
    out
  })
}

/// Called at `run_blocking` boundaries to sync context from an installed fabric.
#[inline]
pub fn ensure_run_context() {
  refresh_adaptive_context();
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compute::{ComputeFabric, ResourcePolicy};

  #[test]
  fn low_budget_raises_threshold() {
    let ctx = AdaptiveContext {
      admission_budget: 1,
      parallelism_threshold: effective_threshold_for_budget(1),
      rayon_threads: 1,
    };
    assert!(ctx.parallelism_threshold >= AdaptiveContext::DEFAULT_THRESHOLD);
  }

  #[test]
  fn high_budget_lowers_threshold() {
    let ctx = AdaptiveContext {
      admission_budget: 8,
      parallelism_threshold: effective_threshold_for_budget(8),
      rayon_threads: 8,
    };
    assert!(ctx.parallelism_threshold < AdaptiveContext::DEFAULT_THRESHOLD);
  }

  #[test]
  fn fabric_install_updates_context() {
    let fabric = Arc::new(ComputeFabric::with_mock(
      ResourcePolicy::memory_cap_max_cpu(0.85),
      0.4,
      0.61,
    ));
    fabric.supervisor().tick();
    install_fabric(Arc::clone(&fabric));
    let ctx = current_adaptive_context();
    assert!(ctx.admission_budget >= 1);
    assert!(ctx.rayon_threads >= 1);
  }

  #[test]
  fn apply_threshold_respects_parallelism_policy() {
    let ctx = AdaptiveContext::standalone();
    assert_eq!(ctx.apply_threshold(Parallelism::Serial), usize::MAX);
    assert_eq!(ctx.apply_threshold(Parallelism::ForceParallel), 0);
    let auto = ctx.apply_threshold(Parallelism::Auto { threshold: 4 });
    assert!(auto <= 4);
  }

  #[test]
  fn effective_threshold_reads_thread_local_context() {
    let ctx = AdaptiveContext {
      admission_budget: 8,
      parallelism_threshold: 2,
      rayon_threads: 8,
    };
    with_adaptive_context(ctx, || {
      assert_eq!(effective_threshold(Parallelism::Auto { threshold: 100 }), 2);
    });
  }

  #[test]
  fn from_fabric_matches_admission_budget() {
    let fabric = ComputeFabric::with_mock(ResourcePolicy::memory_cap_max_cpu(0.85), 0.2, 0.3);
    fabric.supervisor().tick();
    let ctx = AdaptiveContext::from_fabric(&fabric);
    assert!(ctx.admission_budget >= 1);
    assert!(ctx.rayon_threads >= 1);
  }

  #[test]
  fn ensure_run_context_refreshes_without_panic() {
    ensure_run_context();
    let ctx = current_adaptive_context();
    assert!(ctx.rayon_threads >= 1);
  }
}
