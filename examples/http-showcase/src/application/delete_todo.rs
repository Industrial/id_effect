//! [`delete_todo`]: `DELETE /todos/{id}`.

use id_effect::{Effect, Or, err, fail, succeed};

use super::{AppEnv, StoreFailure, with_store};
use crate::domain::todo::{TodoId, TodoNotFound};

/// Removes the todo `id`.
pub fn delete_todo(id: TodoId) -> Effect<(), err!(TodoNotFound | StoreFailure), AppEnv> {
  with_store(move |repo| repo.remove(id))
    .map_err(Or::Right)
    .flat_map(move |removed| {
      if removed {
        succeed(())
      } else {
        fail(Or::Left(TodoNotFound(id)))
      }
    })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::application::{UseCaseHarness, create_todo, list_todos};
  use crate::infrastructure::memory::InMemoryTodoRepository;

  #[tokio::test]
  async fn removes_the_todo_once() {
    let harness = UseCaseHarness::new(InMemoryTodoRepository::new());
    let a = harness.run(create_todo("a".into())).await.unwrap();
    harness.run(create_todo("b".into())).await.unwrap();
    harness.run(delete_todo(a.id())).await.unwrap();
    assert_eq!(harness.run(list_todos()).await.unwrap().len(), 1);
    let out = harness.run(delete_todo(a.id())).await;
    assert!(
      matches!(out, Err(Or::Left(TodoNotFound(id))) if id == a.id()),
      "{out:?}"
    );
  }
}
