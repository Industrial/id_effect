//! Fabric-aware Rayon dispatch for bulk operations.

use crate::parallelism::Parallelism;

/// Run `parallel` when Fabric-aware policy says the input length warrants Rayon; else `serial`.
pub fn parallel_if_profitable<T, S, P>(len: usize, serial: S, parallel: P) -> T
where
  T: Send,
  S: FnOnce() -> T,
  P: FnOnce() -> T + Send,
{
  if Parallelism::default().should_parallelize_current(len) {
    super::rayon_pool::install_parallel(parallel)
  } else {
    serial()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compute::{ComputeFabric, install_fabric};
  use std::sync::Arc;

  #[test]
  fn uses_serial_path_for_tiny_inputs() {
    let n = parallel_if_profitable(1, || 1usize, || 2usize);
    assert_eq!(n, 1);
  }

  #[test]
  fn uses_parallel_path_for_large_inputs() {
    let fabric = Arc::new(ComputeFabric::memory_cap_max_cpu(1.0));
    install_fabric(fabric);
    let n = parallel_if_profitable(4096, || 1usize, || 2usize);
    assert_eq!(n, 2);
  }
}
