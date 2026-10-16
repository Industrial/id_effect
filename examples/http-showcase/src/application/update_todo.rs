//! [`update_todo`]: `PATCH /todos/{id}`.

use id_effect::{Effect, Or, err, fail, succeed};

use super::{AppEnv, StoreFailure, with_store};
use crate::domain::todo::{
  InvalidTitle, Title, TitleTaken, Todo, TodoChanges, TodoId, TodoNotFound,
};

/// Applies the present fields to the todo `id`. The title is validated before the store is
/// called; without changes the todo is returned as stored, without a save.
pub fn update_todo(
  id: TodoId,
  title: Option<String>,
  completed: Option<bool>,
) -> Effect<Todo, err!(InvalidTitle | TodoNotFound | TitleTaken | StoreFailure), AppEnv> {
  let title = match title.as_deref().map(Title::parse).transpose() {
    Ok(title) => title,
    Err(invalid) => return fail(Or::Left(invalid)),
  };
  let changes = TodoChanges { title, completed };
  with_store(move |repo| repo.find(id))
    .map_err(|e| Or::Right(Or::Right(Or::Right(e))))
    .flat_map(move |found| {
      let Some(todo) = found else {
        return fail(Or::Right(Or::Left(TodoNotFound(id))));
      };
      if changes.is_empty() {
        return succeed(todo);
      }
      let todo = todo.apply(changes);
      with_store(move |repo| repo.save(todo.clone()))
        .map_err(|e| Or::Right(Or::Right(Or::Right(e))))
        .flat_map(|saved| match saved {
          Ok(todo) => succeed(todo),
          Err(Or::Left(missing)) => fail(Or::Right(Or::Left(missing))),
          Err(Or::Right(taken)) => fail(Or::Right(Or::Right(Or::Left(taken)))),
        })
    })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::application::{UseCaseHarness, create_todo, get_todo};
  use crate::infrastructure::doubles::FlakyTodoRepository;
  use crate::infrastructure::memory::InMemoryTodoRepository;

  #[tokio::test]
  async fn applies_the_present_fields() {
    let harness = UseCaseHarness::new(InMemoryTodoRepository::new());
    let a = harness.run(create_todo("a".into())).await.unwrap();
    let done = harness
      .run(update_todo(a.id(), None, Some(true)))
      .await
      .unwrap();
    assert!(done.is_completed());
    assert_eq!(done.title().as_str(), "a");
    let renamed = harness
      .run(update_todo(a.id(), Some(" z ".into()), None))
      .await
      .unwrap();
    assert_eq!(renamed.title().as_str(), "z");
    assert!(renamed.is_completed());
    assert_eq!(harness.run(get_todo(a.id())).await.unwrap(), renamed);
  }

  #[tokio::test]
  async fn an_invalid_title_fails_before_the_store_is_called() {
    let repo = FlakyTodoRepository::new(InMemoryTodoRepository::new(), 0);
    let harness = UseCaseHarness::new(repo.clone());
    let out = harness
      .run(update_todo(TodoId::new(1), Some(String::new()), None))
      .await;
    assert!(matches!(out, Err(Or::Left(InvalidTitle::Empty))), "{out:?}");
    assert_eq!(repo.calls(), 0);
  }

  #[tokio::test]
  async fn no_changes_returns_the_todo_without_saving() {
    let repo = FlakyTodoRepository::new(InMemoryTodoRepository::new(), 0);
    let harness = UseCaseHarness::new(repo.clone());
    let a = harness.run(create_todo("a".into())).await.unwrap();
    assert_eq!(
      harness.run(update_todo(a.id(), None, None)).await.unwrap(),
      a
    );
    assert_eq!(repo.calls(), 2);
  }

  #[tokio::test]
  async fn a_missing_todo_or_a_taken_title_fails() {
    let harness = UseCaseHarness::new(InMemoryTodoRepository::new());
    let out = harness
      .run(update_todo(TodoId::new(9), None, Some(true)))
      .await;
    assert!(
      matches!(out, Err(Or::Right(Or::Left(TodoNotFound(_))))),
      "{out:?}"
    );
    harness.run(create_todo("a".into())).await.unwrap();
    let b = harness.run(create_todo("b".into())).await.unwrap();
    let out = harness
      .run(update_todo(b.id(), Some("a".into()), None))
      .await;
    assert!(
      matches!(out, Err(Or::Right(Or::Right(Or::Left(TitleTaken(_)))))),
      "{out:?}"
    );
  }
}
