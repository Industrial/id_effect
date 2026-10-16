//! [`list_todos`]: `GET /todos`.

use id_effect::Effect;

use super::{AppEnv, StoreFailure, with_store};
use crate::domain::todo::Todo;

/// Every todo, by id.
pub fn list_todos() -> Effect<Vec<Todo>, StoreFailure, AppEnv> {
  with_store(|repo| repo.all())
}

#[cfg(test)]
mod tests {
  use std::time::{Duration, Instant};

  use id_effect::Or;

  use super::*;
  use crate::application::{RequestPolicy, RequestTimeout, UseCaseHarness, create_todo};
  use crate::domain::todo::StoreError;
  use crate::infrastructure::doubles::{FlakyTodoRepository, SlowTodoRepository};
  use crate::infrastructure::memory::InMemoryTodoRepository;

  #[tokio::test]
  async fn lists_every_todo_by_id() {
    let harness = UseCaseHarness::new(InMemoryTodoRepository::new());
    harness.run(create_todo("b".into())).await.unwrap();
    harness.run(create_todo("a".into())).await.unwrap();
    let todos = harness.run(list_todos()).await.unwrap();
    let titles: Vec<_> = todos.iter().map(|t| t.title().as_str()).collect();
    assert_eq!(titles, ["b", "a"]);
  }

  #[tokio::test]
  async fn two_transient_failures_are_retried_to_success() {
    let repo = FlakyTodoRepository::new(InMemoryTodoRepository::new(), 2);
    let harness = UseCaseHarness::new(repo.clone());
    assert_eq!(harness.run(list_todos()).await.unwrap(), vec![]);
    assert_eq!(repo.calls(), 3);
  }

  #[tokio::test]
  async fn a_persistent_failure_gives_up_after_the_retries() {
    let repo = FlakyTodoRepository::new(InMemoryTodoRepository::new(), 3);
    let harness = UseCaseHarness::new(repo.clone());
    let out = harness.run(list_todos()).await;
    assert!(
      matches!(out, Err(Or::Right(Or::Right(StoreError::Unavailable(_))))),
      "{out:?}"
    );
    assert_eq!(repo.calls(), 1 + RequestPolicy::DEFAULT_RETRIES as u32);
  }

  #[tokio::test]
  async fn a_slow_store_times_out_with_the_request_timeout() {
    let policy = RequestPolicy::new(
      Duration::from_millis(50),
      RequestPolicy::DEFAULT_RETRIES,
      Duration::from_millis(1),
    );
    let repo = SlowTodoRepository::new(InMemoryTodoRepository::new(), Duration::from_secs(30));
    let harness = UseCaseHarness::with_policy(repo, policy);
    let started = Instant::now();
    let out = harness.run(list_todos()).await;
    assert!(matches!(out, Err(Or::Left(RequestTimeout))), "{out:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
  }

  #[tokio::test]
  async fn an_open_breaker_fails_fast_without_calling_the_store() {
    let repo = FlakyTodoRepository::new(InMemoryTodoRepository::new(), u32::MAX);
    let harness = UseCaseHarness::new(repo.clone());
    // Each request makes up to 3 failing attempts; the breaker opens after 5 in a row.
    for _ in 0..2 {
      assert!(harness.run(list_todos()).await.is_err());
    }
    assert_eq!(repo.calls(), RequestPolicy::BREAKER_THRESHOLD);
    let out = harness.run(list_todos()).await;
    assert!(matches!(out, Err(Or::Right(Or::Left(_)))), "{out:?}");
    assert_eq!(repo.calls(), RequestPolicy::BREAKER_THRESHOLD);
  }
}
