//! [`create_todos`]: `POST /todos/batch`.

use id_effect::{Effect, Or, err, fail, succeed};

use super::{AppEnv, EmptyBatch, StoreFailure, with_store};
use crate::domain::todo::{InvalidTitle, Title, TitleTaken, Todo};

/// Stores a todo for every title, or none. The batch must not be empty and every title must be
/// valid (the first invalid one is reported) before the store is called.
pub fn create_todos(
  titles: Vec<String>,
) -> Effect<Vec<Todo>, err!(EmptyBatch | InvalidTitle | TitleTaken | StoreFailure), AppEnv> {
  if titles.is_empty() {
    return fail(Or::Left(EmptyBatch));
  }
  let titles = match titles
    .iter()
    .map(|t| Title::parse(t))
    .collect::<Result<Vec<_>, _>>()
  {
    Ok(titles) => titles,
    Err(invalid) => return fail(Or::Right(Or::Left(invalid))),
  };
  with_store(move |repo| repo.add_all(titles.clone()))
    .map_err(|e| Or::Right(Or::Right(Or::Right(e))))
    .flat_map(|added| match added {
      Ok(todos) => succeed(todos),
      Err(taken) => fail(Or::Right(Or::Right(Or::Left(taken)))),
    })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::application::{UseCaseHarness, list_todos};
  use crate::infrastructure::doubles::FlakyTodoRepository;
  use crate::infrastructure::memory::InMemoryTodoRepository;

  #[tokio::test]
  async fn stores_every_title() {
    let harness = UseCaseHarness::new(InMemoryTodoRepository::new());
    let both = harness
      .run(create_todos(vec!["b".into(), "c".into()]))
      .await
      .unwrap();
    assert_eq!(both.len(), 2);
    assert_eq!(harness.run(list_todos()).await.unwrap(), both);
  }

  #[tokio::test]
  async fn inputs_are_validated_before_the_store_is_called() {
    let repo = FlakyTodoRepository::new(InMemoryTodoRepository::new(), 0);
    let harness = UseCaseHarness::new(repo.clone());
    let out = harness.run(create_todos(vec![])).await;
    assert!(matches!(out, Err(Or::Left(EmptyBatch))), "{out:?}");
    let out = harness
      .run(create_todos(vec!["ok".into(), String::new()]))
      .await;
    assert!(
      matches!(out, Err(Or::Right(Or::Left(InvalidTitle::Empty)))),
      "{out:?}"
    );
    assert_eq!(repo.calls(), 0);
  }

  #[tokio::test]
  async fn a_taken_title_stores_nothing() {
    let harness = UseCaseHarness::new(InMemoryTodoRepository::new());
    let out = harness
      .run(create_todos(vec!["a".into(), "a".into()]))
      .await;
    assert!(
      matches!(out, Err(Or::Right(Or::Right(Or::Left(TitleTaken(_)))))),
      "{out:?}"
    );
    assert!(harness.run(list_todos()).await.unwrap().is_empty());
  }
}
