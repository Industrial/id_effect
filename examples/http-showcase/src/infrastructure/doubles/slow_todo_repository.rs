//! [`SlowTodoRepository`]: a store that answers late.

use std::time::Duration;

use id_effect::{Effect, sleep};

use crate::domain::todo::{
  StoreError, Title, TitleTaken, Todo, TodoId, TodoNotFound, TodoRepository,
};
use crate::infrastructure::memory::InMemoryTodoRepository;

/// Sleeps `delay` (interruptibly, on the fiber's clock) before every call, then delegates to
/// `inner`.
#[derive(Clone, Debug)]
pub(crate) struct SlowTodoRepository {
  inner: InMemoryTodoRepository,
  delay: Duration,
}

impl SlowTodoRepository {
  /// `inner`, `delay` late on every call.
  pub(crate) fn new(inner: InMemoryTodoRepository, delay: Duration) -> Self {
    Self { inner, delay }
  }

  /// Waits `delay`.
  fn before(&self) -> Effect<(), StoreError> {
    sleep(self.delay)
  }
}

delegate_repository!(SlowTodoRepository, before);
