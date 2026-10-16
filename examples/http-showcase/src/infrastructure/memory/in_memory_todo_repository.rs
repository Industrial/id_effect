//! [`InMemoryTodoRepository`]: a [`TodoRepository`] in process memory.

use std::sync::{Arc, Mutex, PoisonError};

use id_effect::{Effect, Or, sync};

use super::TodoStore;
use crate::domain::todo::{
  StoreError, Title, TitleTaken, Todo, TodoId, TodoNotFound, TodoRepository,
};

/// A [`TodoRepository`] in process memory (lost on restart), used without `DATABASE_URL` and by
/// the tests. It never fails with a [`StoreError`]. Clones share the store.
#[derive(Clone, Debug, Default)]
pub struct InMemoryTodoRepository {
  store: Arc<Mutex<TodoStore>>,
}

impl InMemoryTodoRepository {
  /// An empty store.
  pub fn new() -> Self {
    Self::default()
  }

  /// Runs `f` on the store when the effect runs.
  fn with<A: Send + 'static>(
    &self,
    f: impl FnOnce(&mut TodoStore) -> A + Send + 'static,
  ) -> Effect<A, StoreError> {
    let store = Arc::clone(&self.store);
    sync(move || f(&mut store.lock().unwrap_or_else(PoisonError::into_inner)))
  }
}

impl TodoRepository for InMemoryTodoRepository {
  fn all(&self) -> Effect<Vec<Todo>, StoreError> {
    self.with(|s| s.all())
  }

  fn find(&self, id: TodoId) -> Effect<Option<Todo>, StoreError> {
    self.with(move |s| s.find(id))
  }

  fn add(&self, title: Title) -> Effect<Result<Todo, TitleTaken>, StoreError> {
    self.with(move |s| s.add(title))
  }

  fn add_all(&self, titles: Vec<Title>) -> Effect<Result<Vec<Todo>, TitleTaken>, StoreError> {
    self.with(move |s| s.add_all(titles))
  }

  fn save(&self, todo: Todo) -> Effect<Result<Todo, Or<TodoNotFound, TitleTaken>>, StoreError> {
    self.with(move |s| s.save(todo))
  }

  fn remove(&self, id: TodoId) -> Effect<bool, StoreError> {
    self.with(move |s| s.remove(id))
  }
}

#[cfg(test)]
mod tests {
  use id_effect::Exit;

  use super::*;

  fn run<A: Send + 'static>(eff: Effect<A, StoreError>) -> A {
    match eff.block_on() {
      Exit::Success(a) => a,
      Exit::Failure(cause) => panic!("{cause:?}"),
    }
  }

  fn title(raw: &str) -> Title {
    Title::parse(raw).unwrap()
  }

  #[test]
  fn stores_finds_saves_and_removes() {
    let repo = InMemoryTodoRepository::new();
    let milk = run(repo.add(title("milk"))).unwrap();
    assert_eq!(milk.id(), TodoId::new(1));
    assert_eq!(run(repo.find(milk.id())), Some(milk.clone()));
    assert_eq!(run(repo.add(title("milk"))), Err(TitleTaken(title("milk"))));
    let mut done = milk.clone();
    done.complete();
    assert_eq!(run(repo.save(done.clone())), Ok(done.clone()));
    assert!(run(repo.remove(milk.id())));
    assert!(!run(repo.remove(milk.id())));
    assert_eq!(
      run(repo.save(done)),
      Err(Or::Left(TodoNotFound(TodoId::new(1))))
    );
  }

  #[test]
  fn add_all_is_all_or_nothing() {
    let repo = InMemoryTodoRepository::new();
    run(repo.add(title("a"))).unwrap();
    assert_eq!(
      run(repo.add_all(vec![title("b"), title("a")])),
      Err(TitleTaken(title("a")))
    );
    assert_eq!(run(repo.all()).len(), 1);
    assert_eq!(
      run(repo.add_all(vec![title("c"), title("c")])),
      Err(TitleTaken(title("c")))
    );
    assert_eq!(
      run(repo.add_all(vec![title("b"), title("c")]))
        .unwrap()
        .len(),
      2
    );
    assert_eq!(run(repo.all()).len(), 3);
  }

  #[test]
  fn save_rejects_a_title_held_by_another_todo() {
    let repo = InMemoryTodoRepository::new();
    run(repo.add(title("a"))).unwrap();
    let b = run(repo.add(title("b"))).unwrap();
    let renamed = b.apply(crate::domain::todo::TodoChanges {
      title: Some(title("a")),
      completed: None,
    });
    assert_eq!(
      run(repo.save(renamed)),
      Err(Or::Right(TitleTaken(title("a"))))
    );
  }
}
