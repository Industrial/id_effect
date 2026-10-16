//! [`with_store`]: one store call of a use case, run under the request policy.

use id_effect::{Effect, effect};

use super::{AppEnv, RequestPolicy, StoreFailure};
use crate::domain::todo::{StoreError, TodoRepository, TodoRepositoryService};

/// Runs `call` on the [`TodoRepositoryService`] capability through [`RequestPolicy::run`]. `call`
/// is the attempt factory: it is invoked once per attempt, so it clones what it captures.
pub(crate) fn with_store<A: Send + 'static>(
  call: impl Fn(&dyn TodoRepository) -> Effect<A, StoreError> + Send + Sync + 'static,
) -> Effect<A, StoreFailure, AppEnv> {
  effect!(|_r: &AppEnv| {
    let repo = TodoRepositoryService::clone(~TodoRepositoryService);
    let policy = RequestPolicy::clone(~RequestPolicy);
    ~policy.run(move || call(&*repo))
  })
}
