//! [`UseCaseHarness`]: runs use cases against a test store (tests only).

use std::fmt::Debug;
use std::time::Duration;

use id_effect::{Cause, Effect, Exit};
use id_effect_axum::EffectExecutor;

use super::{AppEnv, RequestPolicy, new_app_env};
use crate::domain::todo::{TodoRepository, TodoRepositoryService};

/// A store and a request policy; every run shares both (so the policy's circuit breaker sees
/// every call).
pub(crate) struct UseCaseHarness {
  repository: TodoRepositoryService,
  policy: RequestPolicy,
}

impl UseCaseHarness {
  /// `repository` under a 5 s timeout, [`RequestPolicy::DEFAULT_RETRIES`] and a 1 ms backoff.
  pub(crate) fn new(repository: impl TodoRepository) -> Self {
    let policy = RequestPolicy::new(
      Duration::from_secs(5),
      RequestPolicy::DEFAULT_RETRIES,
      Duration::from_millis(1),
    );
    Self::with_policy(repository, policy)
  }

  /// `repository` under `policy`.
  pub(crate) fn with_policy(repository: impl TodoRepository, policy: RequestPolicy) -> Self {
    Self {
      repository: TodoRepositoryService::new(repository),
      policy,
    }
  }

  /// Runs `use_case` to its value or typed failure; any other cause panics.
  pub(crate) async fn run<A, E>(&self, use_case: Effect<A, E, AppEnv>) -> Result<A, E>
  where
    A: Send + 'static,
    E: Debug + Send + 'static,
  {
    let env = new_app_env(self.repository.clone(), self.policy.clone());
    match EffectExecutor::global()
      .run_effect(use_case.provide_all(env))
      .await
    {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(cause) => panic!("unexpected cause: {cause:?}"),
    }
  }
}
