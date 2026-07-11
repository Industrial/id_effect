//! Optional Rayon thread pool sized by the Compute supervisor.

use std::sync::{Mutex, OnceLock};

use rayon::ThreadPool;

static POOL: OnceLock<Mutex<Option<ThreadPool>>> = OnceLock::new();

fn pool_slot() -> &'static Mutex<Option<ThreadPool>> {
  POOL.get_or_init(|| Mutex::new(None))
}

/// Configure (or rebuild) the fabric-scoped Rayon pool with `threads` workers.
pub fn configure_rayon_threads(threads: usize) {
  let threads = threads.max(1);
  let pool = rayon::ThreadPoolBuilder::new()
    .num_threads(threads)
    .build()
    .expect("rayon ThreadPoolBuilder");
  *pool_slot().lock().expect("rayon pool mutex") = Some(pool);
}

/// Run `f` on the fabric pool when configured; otherwise uses the global Rayon pool.
pub fn install_parallel<F, R>(f: F) -> R
where
  F: FnOnce() -> R + Send,
  R: Send,
{
  let guard = pool_slot().lock().expect("rayon pool mutex");
  if let Some(pool) = guard.as_ref() {
    pool.install(f)
  } else {
    f()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn configure_and_install_parallel() {
    configure_rayon_threads(2);
    let threads = install_parallel(|| rayon::current_num_threads());
    assert_eq!(threads, 2);
  }

  #[test]
  fn install_parallel_without_pool_runs_inline() {
    *pool_slot().lock().expect("rayon pool mutex") = None;
    let out = install_parallel(|| 42);
    assert_eq!(out, 42);
  }

  #[test]
  fn configure_rebuilds_pool_for_new_thread_count() {
    configure_rayon_threads(3);
    let first = install_parallel(|| rayon::current_num_threads());
    configure_rayon_threads(1);
    let second = install_parallel(|| rayon::current_num_threads());
    assert_eq!(first, 3);
    assert_eq!(second, 1);
  }
}
