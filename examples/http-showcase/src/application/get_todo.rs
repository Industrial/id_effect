//! [`get_todo`]: `GET /todos/{id}`.

use id_effect::{Effect, Or, err, fail, succeed};

use super::{AppEnv, StoreFailure, with_store};
use crate::domain::todo::{Todo, TodoId, TodoNotFound};

/// The todo `id`.
pub fn get_todo(id: TodoId) -> Effect<Todo, err!(TodoNotFound | StoreFailure), AppEnv> {
  with_store(move |repo| repo.find(id))
    .map_err(Or::Right)
    .flat_map(move |found| match found {
      Some(todo) => succeed(todo),
      None => fail(Or::Left(TodoNotFound(id))),
    })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::application::{UseCaseHarness, create_todo};
  use crate::infrastructure::doubles::FlakyTodoRepository;
  use crate::infrastructure::memory::InMemoryTodoRepository;

  #[tokio::test]
  async fn finds_a_stored_todo() {
    let harness = UseCaseHarness::new(InMemoryTodoRepository::new());
    let milk = harness.run(create_todo("milk".into())).await.unwrap();
    assert_eq!(harness.run(get_todo(milk.id())).await.unwrap(), milk);
  }

  #[tokio::test]
  async fn a_missing_todo_is_a_domain_outcome_and_not_retried() {
    let repo = FlakyTodoRepository::new(InMemoryTodoRepository::new(), 0);
    let harness = UseCaseHarness::new(repo.clone());
    let out = harness.run(get_todo(TodoId::new(4))).await;
    assert!(
      matches!(out, Err(Or::Left(TodoNotFound(id))) if id == TodoId::new(4)),
      "{out:?}"
    );
    assert_eq!(repo.calls(), 1);
  }
}
