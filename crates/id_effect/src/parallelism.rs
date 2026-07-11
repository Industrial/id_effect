//! Execution policy for bulk data-parallel operations (Rayon).

use crate::compute::AdaptiveContext;

/// Controls when collection and stream chunk operations use Rayon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Parallelism {
  /// Use parallel execution when `len >= threshold`.
  Auto {
    /// Minimum element count before switching to Rayon.
    threshold: usize,
  },
  /// Always use Rayon when the operation supports it.
  ForceParallel,
  /// Never use Rayon.
  Serial,
}

impl Parallelism {
  /// Default auto threshold (1024 elements).
  pub const DEFAULT_THRESHOLD: usize = 1024;

  /// Returns whether to use a parallel code path for `len` elements.
  #[inline]
  pub fn should_parallelize(self, len: usize) -> bool {
    match self {
      Self::Serial => false,
      Self::ForceParallel => true,
      Self::Auto { threshold } => len >= threshold,
    }
  }

  /// Hardware-aware threshold from an [`AdaptiveContext`] snapshot.
  #[inline]
  pub fn effective_threshold(self, ctx: &AdaptiveContext) -> usize {
    ctx.apply_threshold(self)
  }

  /// Whether to parallelize `len` elements using fabric-aware thresholds.
  #[inline]
  pub fn should_parallelize_adaptive(self, len: usize, ctx: &AdaptiveContext) -> bool {
    match self {
      Self::Serial => false,
      Self::ForceParallel => true,
      Self::Auto { .. } => len >= self.effective_threshold(ctx),
    }
  }

  /// Like [`Self::should_parallelize`] but reads the thread-local adaptive context.
  #[inline]
  pub fn should_parallelize_current(self, len: usize) -> bool {
    self.should_parallelize_adaptive(len, &crate::compute::current_adaptive_context())
  }
}

impl Default for Parallelism {
  fn default() -> Self {
    Self::Auto {
      threshold: Self::DEFAULT_THRESHOLD,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compute::AdaptiveContext;

  #[test]
  fn auto_below_threshold_is_serial() {
    assert!(!Parallelism::Auto { threshold: 10 }.should_parallelize(9));
  }

  #[test]
  fn auto_at_threshold_is_parallel() {
    assert!(Parallelism::Auto { threshold: 10 }.should_parallelize(10));
  }

  #[test]
  fn force_parallel_always() {
    assert!(Parallelism::ForceParallel.should_parallelize(0));
  }

  #[test]
  fn serial_never() {
    assert!(!Parallelism::Serial.should_parallelize(usize::MAX));
  }

  #[test]
  fn default_is_auto_1024() {
    assert_eq!(
      Parallelism::default(),
      Parallelism::Auto { threshold: 1024 }
    );
  }

  #[test]
  fn adaptive_lowers_threshold_under_headroom() {
    let ctx = AdaptiveContext {
      admission_budget: 8,
      parallelism_threshold: 256,
      rayon_threads: 8,
    };
    assert!(Parallelism::default().should_parallelize_adaptive(300, &ctx));
    assert!(!Parallelism::default().should_parallelize_adaptive(100, &ctx));
  }
}
