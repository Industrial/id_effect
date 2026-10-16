//! [`TodoRepositoryService`]: the [`TodoRepository`] port as a capability.

use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use super::TodoRepository;

/// Capability: the application's [`TodoRepository`] (derefs to `dyn TodoRepository`).
#[derive(Clone)]
pub struct TodoRepositoryService(pub Arc<dyn TodoRepository>);

impl TodoRepositoryService {
  /// Wraps `repository` as the capability.
  pub fn new(repository: impl TodoRepository) -> Self {
    Self(Arc::new(repository))
  }
}

impl Deref for TodoRepositoryService {
  type Target = dyn TodoRepository;

  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl fmt::Debug for TodoRepositoryService {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("TodoRepositoryService")
      .field(&"<dyn TodoRepository>")
      .finish()
  }
}
