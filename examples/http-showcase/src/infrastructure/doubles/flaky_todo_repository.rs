//! [`FlakyTodoRepository`]: a store that is unavailable for its first calls.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use id_effect::{Effect, fail, succeed, suspend};

use crate::domain::todo::{
  StoreError, Title, TitleTaken, Todo, TodoId, TodoNotFound, TodoRepository,
};
use crate::infrastructure::memory::InMemoryTodoRepository;

/// Fails its first `failures` calls (of any method) with [`StoreError::Unavailable`], then
/// delegates to `inner`. Counts every call; clones share the count.
#[derive(Clone, Debug)]
pub(crate) struct FlakyTodoRepository {
  inner: InMemoryTodoRepository,
  failures: u32,
  calls: Arc<AtomicU32>,
}

impl FlakyTodoRepository {
  /// `inner`, unavailable for its first `failures` calls.
  pub(crate) fn new(inner: InMemoryTodoRepository, failures: u32) -> Self {
    Self {
      inner,
      failures,
      calls: Arc::new(AtomicU32::new(0)),
    }
  }

  /// How many calls have run (failed ones included).
  pub(crate) fn calls(&self) -> u32 {
    self.calls.load(Ordering::SeqCst)
  }

  /// Counts the call when it runs; fails while failures are left.
  fn before(&self) -> Effect<(), StoreError> {
    let (calls, failures) = (Arc::clone(&self.calls), self.failures);
    suspend(move || {
      if calls.fetch_add(1, Ordering::SeqCst) < failures {
        fail(StoreError::Unavailable("flaky store".into()))
      } else {
        succeed(())
      }
    })
  }
}

delegate_repository!(FlakyTodoRepository, before);
