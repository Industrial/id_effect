//! [`delegate_repository!`]: implements the port for a wrapper over a store.

/// Implements [`TodoRepository`](crate::domain::todo::TodoRepository) for `$ty` (a struct with an
/// `inner` [`TodoRepository`](crate::domain::todo::TodoRepository) field): every method runs
/// `self.$before()` (an `Effect<(), StoreError>`) and then delegates to `inner`. The caller
/// imports `TodoRepository`, `StoreError`, `Title`, `TitleTaken`, `Todo`, `TodoId` and
/// `TodoNotFound`.
macro_rules! delegate_repository {
  ($ty:ty, $before:ident) => {
    impl TodoRepository for $ty {
      fn all(&self) -> ::id_effect::Effect<Vec<Todo>, StoreError> {
        let inner = self.inner.clone();
        self.$before().flat_map(move |()| inner.all())
      }

      fn find(&self, id: TodoId) -> ::id_effect::Effect<Option<Todo>, StoreError> {
        let inner = self.inner.clone();
        self.$before().flat_map(move |()| inner.find(id))
      }

      fn add(&self, title: Title) -> ::id_effect::Effect<Result<Todo, TitleTaken>, StoreError> {
        let inner = self.inner.clone();
        self.$before().flat_map(move |()| inner.add(title))
      }

      fn add_all(
        &self,
        titles: Vec<Title>,
      ) -> ::id_effect::Effect<Result<Vec<Todo>, TitleTaken>, StoreError> {
        let inner = self.inner.clone();
        self.$before().flat_map(move |()| inner.add_all(titles))
      }

      fn save(
        &self,
        todo: Todo,
      ) -> ::id_effect::Effect<Result<Todo, ::id_effect::Or<TodoNotFound, TitleTaken>>, StoreError>
      {
        let inner = self.inner.clone();
        self.$before().flat_map(move |()| inner.save(todo))
      }

      fn remove(&self, id: TodoId) -> ::id_effect::Effect<bool, StoreError> {
        let inner = self.inner.clone();
        self.$before().flat_map(move |()| inner.remove(id))
      }
    }
  };
}
