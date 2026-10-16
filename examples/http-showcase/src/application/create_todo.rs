//! [`create_todo`]: `POST /todos`.

use id_effect::{Effect, Or, err, fail, succeed};

use super::{AppEnv, StoreFailure, with_store};
use crate::domain::todo::{InvalidTitle, Title, TitleTaken, Todo};

/// Stores a new todo titled `title` (validated into a [`Title`] before the store is called).
pub fn create_todo(
  title: String,
) -> Effect<Todo, err!(InvalidTitle | TitleTaken | StoreFailure), AppEnv> {
  let title = match Title::parse(&title) {
    Ok(title) => title,
    Err(invalid) => return fail(Or::Left(invalid)),
  };
  with_store(move |repo| repo.add(title.clone()))
    .map_err(|e| Or::Right(Or::Right(e)))
    .flat_map(|added| match added {
      Ok(todo) => succeed(todo),
      Err(taken) => fail(Or::Right(Or::Left(taken))),
    })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::application::UseCaseHarness;
  use crate::infrastructure::doubles::FlakyTodoRepository;
  use crate::infrastructure::memory::InMemoryTodoRepository;

  #[tokio::test]
  async fn stores_a_trimmed_open_todo() {
    let harness = UseCaseHarness::new(InMemoryTodoRepository::new());
    let todo = harness.run(create_todo(" a ".into())).await.unwrap();
    assert_eq!(todo.title().as_str(), "a");
    assert!(!todo.is_completed());
  }

  #[tokio::test]
  async fn an_invalid_title_fails_before_the_store_is_called() {
    let repo = FlakyTodoRepository::new(InMemoryTodoRepository::new(), 0);
    let harness = UseCaseHarness::new(repo.clone());
    let out = harness.run(create_todo("  ".into())).await;
    assert!(matches!(out, Err(Or::Left(InvalidTitle::Empty))), "{out:?}");
    assert_eq!(repo.calls(), 0);
  }

  #[tokio::test]
  async fn a_taken_title_fails() {
    let harness = UseCaseHarness::new(InMemoryTodoRepository::new());
    harness.run(create_todo("a".into())).await.unwrap();
    let out = harness.run(create_todo("a".into())).await;
    assert!(
      matches!(out, Err(Or::Right(Or::Left(TitleTaken(_))))),
      "{out:?}"
    );
  }
}
