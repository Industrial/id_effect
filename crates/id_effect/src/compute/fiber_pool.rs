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
