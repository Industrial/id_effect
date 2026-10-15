//! The application layer: the environment every `/todos` handler runs in ([`AppEnv`]), the
//! [`RequestPolicy`] each store call runs under, and the use cases (one effect per endpoint).
//!
//! Every use case validates its input, then calls the [`TodoRepoService`] capability through
//! [`RequestPolicy::run`]:
//!
//! - **retry**: a transient failure ([`AppError::is_transient`]: the store is unavailable) is
//!   retried `retries` times with exponential backoff (Effect.ts
//!   `Effect.retry(Schedule.exponential(..).pipe(Schedule.intersect(Schedule.recurs(n)),
//!   Schedule.whileInput(isTransient)))`). Any other failure stops at once.
//! - **timeout**: the whole call, retries included, fails with [`AppError::Timeout`] (`504`)
//!   once `REQUEST_TIMEOUT` ([`AppConfig::request_timeout`]) has passed; the store's fiber is
//!   interrupted.

use std::time::Duration;

use id_effect::{Effect, HCons, HNil, Schedule, effect, fail, hlist, suspend};

use crate::config::AppConfig;
use crate::error::AppError;
use crate::repo::{Title, Todo, TodoPatch, TodoRepo, TodoRepoService};

/// Retries of a transient store failure (so up to `1 + DEFAULT_RETRIES` attempts).
pub const DEFAULT_RETRIES: u64 = 2;

/// Delay before the first retry; it doubles for each further one.
pub const DEFAULT_BACKOFF: Duration = Duration::from_millis(10);

/// How each store call of a request runs: bounded by a timeout, transient failures retried.
#[derive(Clone, Debug)]
pub struct RequestPolicy {
  timeout: Duration,
  retry: Schedule<(Duration, u64), AppError>,
}

impl RequestPolicy {
  /// Calls bounded by `timeout`; up to `retries` retries of a transient failure, `backoff`,
  /// `2 * backoff`, .. apart.
  pub fn new(timeout: Duration, retries: u64, backoff: Duration) -> Self {
    let retry = Schedule::exponential(backoff)
      .intersect(Schedule::recurs(retries))
      .while_input(AppError::is_transient);
    Self { timeout, retry }
  }

  /// The policy for `config`: its `REQUEST_TIMEOUT`, [`DEFAULT_RETRIES`], [`DEFAULT_BACKOFF`].
  pub fn from_config(config: &AppConfig) -> Self {
    Self::new(config.request_timeout, DEFAULT_RETRIES, DEFAULT_BACKOFF)
  }

  /// The per-request timeout.
  pub fn timeout(&self) -> Duration {
    self.timeout
  }

  /// Runs `attempt()` under this policy. `attempt` is the call's factory: an effect runs
  /// once, so every retry builds a fresh one.
  pub fn run<A: Send + 'static>(
    &self,
    attempt: impl Fn() -> Effect<A, AppError> + Send + Sync + 'static,
  ) -> Effect<A, AppError> {
    Effect::retry(attempt, self.retry.clone()).timeout_fail(self.timeout, || AppError::Timeout)
  }
}

/// The environment of every `/todos` handler: the store and the request policy. It is the
/// router's state (`id_effect_axum` provides it to each request's effect).
pub type AppEnv = HCons<TodoRepoService, HCons<RequestPolicy, HNil>>;

/// The [`AppEnv`] for `repo` and `policy`.
pub fn app_env(repo: TodoRepoService, policy: RequestPolicy) -> AppEnv {
  hlist![repo, policy]
}

/// Runs `call` on the store capability under the request policy.
fn with_repo<A: Send + 'static>(
  call: impl Fn(&dyn TodoRepo) -> Effect<A, AppError> + Send + Sync + 'static,
) -> Effect<A, AppError, AppEnv> {
  effect!(|_r: &AppEnv| {
    let repo = TodoRepoService::clone(~TodoRepoService);
    let policy = RequestPolicy::clone(~RequestPolicy);
    ~policy.run(move || call(&*repo))
  })
}

/// `input` validated, then `then(valid)`; an invalid input fails with its error.
fn validated<T: Send + 'static, A: Send + 'static>(
  input: Result<T, AppError>,
  then: impl FnOnce(T) -> Effect<A, AppError, AppEnv> + Send + 'static,
) -> Effect<A, AppError, AppEnv> {
  suspend(move || match input {
    Ok(valid) => then(valid),
    Err(e) => fail(e),
  })
}

/// `GET /todos`.
pub fn list_todos() -> Effect<Vec<Todo>, AppError, AppEnv> {
  with_repo(|repo| repo.list())
}

/// `GET /todos/{id}`.
pub fn get_todo(id: i64) -> Effect<Todo, AppError, AppEnv> {
  with_repo(move |repo| repo.get(id))
}

/// `POST /todos`.
pub fn create_todo(title: &str) -> Effect<Todo, AppError, AppEnv> {
  validated(Title::parse(title), |title| {
    with_repo(move |repo| repo.create(title.clone()))
  })
}

/// `POST /todos/batch`: every title or none.
pub fn create_todos(titles: &[String]) -> Effect<Vec<Todo>, AppError, AppEnv> {
  let titles = if titles.is_empty() {
    Err(AppError::Invalid("the batch must not be empty".into()))
  } else {
    titles.iter().map(|t| Title::parse(t)).collect()
  };
  validated(titles, |titles: Vec<Title>| {
    with_repo(move |repo| repo.create_many(titles.clone()))
  })
}

/// `PATCH /todos/{id}`.
pub fn update_todo(
  id: i64,
  title: Option<&str>,
  completed: Option<bool>,
) -> Effect<Todo, AppError, AppEnv> {
  let patch = title
    .map(Title::parse)
    .transpose()
    .map(|title| TodoPatch { title, completed });
  validated(patch, move |patch| {
    with_repo(move |repo| repo.update(id, patch.clone()))
  })
}

/// `DELETE /todos/{id}`.
pub fn delete_todo(id: i64) -> Effect<(), AppError, AppEnv> {
  with_repo(move |repo| repo.delete(id))
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit};
  use id_effect_axum::EffectExecutor;
  use id_effect_config::MapConfigProvider;

  use super::*;
  use crate::repo::InMemoryTodoRepo;
  use crate::repo::doubles::{FlakyRepo, SlowRepo};

  fn policy() -> RequestPolicy {
    RequestPolicy::new(
      Duration::from_secs(5),
      DEFAULT_RETRIES,
      Duration::from_millis(1),
    )
  }

  async fn run<A: Send + 'static>(
    eff: Effect<A, AppError, AppEnv>,
    repo: impl TodoRepo,
    policy: RequestPolicy,
  ) -> Result<A, AppError> {
    let program = eff.provide_all(app_env(TodoRepoService::new(repo), policy));
    match EffectExecutor::global().run_effect(program).await {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  #[tokio::test]
  async fn two_transient_failures_are_retried_to_success() {
    let repo = FlakyRepo::new(InMemoryTodoRepo::new(), 2);
    assert_eq!(run(list_todos(), repo.clone(), policy()).await, Ok(vec![]));
    assert_eq!(repo.calls(), 3);
  }

  #[tokio::test]
  async fn a_persistent_failure_gives_up_after_the_retries() {
    let repo = FlakyRepo::new(InMemoryTodoRepo::new(), 3);
    let out = run(list_todos(), repo.clone(), policy()).await;
    assert!(matches!(out, Err(AppError::Unavailable(_))), "{out:?}");
    assert_eq!(repo.calls(), 1 + DEFAULT_RETRIES as u32);
  }

  #[tokio::test]
  async fn a_non_transient_failure_is_not_retried() {
    let repo = FlakyRepo::new(InMemoryTodoRepo::new(), 0);
    assert_eq!(
      run(get_todo(4), repo.clone(), policy()).await,
      Err(AppError::NotFound(4))
    );
    assert_eq!(repo.calls(), 1);
  }

  #[tokio::test]
  async fn a_slow_store_times_out_with_the_configured_request_timeout() {
    let config = AppConfig::config()
      .load(&MapConfigProvider::from_pairs([(
        "REQUEST_TIMEOUT",
        "50ms",
      )]))
      .expect("config");
    let policy = RequestPolicy::from_config(&config);
    assert_eq!(policy.timeout(), Duration::from_millis(50));
    let repo = SlowRepo::new(InMemoryTodoRepo::new(), Duration::from_secs(30));
    let started = std::time::Instant::now();
    assert_eq!(
      run(list_todos(), repo, policy).await,
      Err(AppError::Timeout)
    );
    assert!(started.elapsed() < Duration::from_secs(5));
  }

  #[tokio::test]
  async fn inputs_are_validated_before_the_store_is_called() {
    let repo = FlakyRepo::new(InMemoryTodoRepo::new(), 0);
    assert!(matches!(
      run(create_todo("  "), repo.clone(), policy()).await,
      Err(AppError::Invalid(_))
    ));
    assert!(matches!(
      run(create_todos(&[]), repo.clone(), policy()).await,
      Err(AppError::Invalid(_))
    ));
    assert!(matches!(
      run(
        create_todos(&["ok".into(), String::new()]),
        repo.clone(),
        policy()
      )
      .await,
      Err(AppError::Invalid(_))
    ));
    assert!(matches!(
      run(update_todo(1, Some(""), None), repo.clone(), policy()).await,
      Err(AppError::Invalid(_))
    ));
    assert_eq!(repo.calls(), 0);
  }

  #[tokio::test]
  async fn use_cases_reach_the_store() {
    let repo = InMemoryTodoRepo::new();
    let a = run(create_todo(" a "), repo.clone(), policy())
      .await
      .unwrap();
    assert_eq!(a.title, "a");
    let both = run(
      create_todos(&["b".into(), "c".into()]),
      repo.clone(),
      policy(),
    )
    .await
    .unwrap();
    assert_eq!(both.len(), 2);
    let done = run(update_todo(a.id, None, Some(true)), repo.clone(), policy())
      .await
      .unwrap();
    assert!(done.completed);
    assert_eq!(run(get_todo(a.id), repo.clone(), policy()).await, Ok(done));
    run(delete_todo(a.id), repo.clone(), policy())
      .await
      .unwrap();
    assert_eq!(run(list_todos(), repo, policy()).await.unwrap().len(), 2);
  }
}
