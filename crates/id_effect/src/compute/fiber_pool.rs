//! Work-stealing-style fiber worker pool (job queue + fixed workers).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};

type Job = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug)]
struct PoolInner {
  jobs: flume::Sender<Job>,
  target_size: AtomicUsize,
}

/// Fixed worker pool for fiber execution.
#[derive(Debug, Clone)]
pub struct FiberPool {
  inner: Arc<PoolInner>,
  _workers: Arc<Vec<JoinHandle<()>>>,
}

impl FiberPool {
  pub fn new(size: usize) -> Self {
    let size = size.max(1);
    let (tx, rx) = flume::unbounded::<Job>();
    let inner = Arc::new(PoolInner {
      jobs: tx,
      target_size: AtomicUsize::new(size),
    });
    let mut workers = Vec::with_capacity(size);
    for _ in 0..size {
      let rx = rx.clone();
      workers.push(thread::spawn(move || {
        while let Ok(job) = rx.recv() {
          job();
        }
      }));
    }
    Self {
      inner,
      _workers: Arc::new(workers),
    }
  }

  pub fn default_size() -> usize {
    std::thread::available_parallelism()
      .map(|n| n.get())
      .unwrap_or(4)
      .max(1)
  }

  pub fn target_size(&self) -> usize {
    self.inner.target_size.load(Ordering::Relaxed)
  }

  pub fn set_target_size(&self, size: usize) {
    self.inner.target_size.store(size.max(1), Ordering::Relaxed);
  }

  pub fn spawn<F>(&self, f: F)
  where
    F: FnOnce() + Send + 'static,
  {
    let _ = self.inner.jobs.send(Box::new(f));
  }
}

impl Default for FiberPool {
  fn default() -> Self {
    Self::new(Self::default_size())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::sync::{Arc, mpsc};
  use std::time::Duration;

  #[test]
  fn default_size_is_at_least_one() {
    assert!(FiberPool::default_size() >= 1);
  }

  #[test]
  fn spawn_runs_job_on_worker() {
    let pool = FiberPool::new(2);
    let (tx, rx) = mpsc::channel();
    pool.spawn(move || tx.send(()).unwrap());
    rx.recv_timeout(Duration::from_secs(2)).expect("job ran");
  }

  #[test]
  fn target_size_tracks_updates() {
    let pool = FiberPool::new(2);
    assert_eq!(pool.target_size(), 2);
    pool.set_target_size(5);
    assert_eq!(pool.target_size(), 5);
    pool.set_target_size(0);
    assert_eq!(pool.target_size(), 1);
  }

  #[test]
  fn default_pool_executes_multiple_jobs() {
    let pool = FiberPool::default();
    let done = Arc::new(AtomicUsize::new(0));
    for _ in 0..4 {
      let done = Arc::clone(&done);
      pool.spawn(move || {
        done.fetch_add(1, Ordering::SeqCst);
      });
    }
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(done.load(Ordering::SeqCst), 4);
  }
}
