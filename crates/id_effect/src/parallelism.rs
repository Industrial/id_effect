//! Execution policy for bulk data-parallel operations (Rayon).

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
}
