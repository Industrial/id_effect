//! [`TodoRepository`]: the port through which the application stores todos.

use id_effect::{Effect, Or};

use super::{StoreError, Title, TitleTaken, Todo, TodoId, TodoNotFound};

/// The todo store (Effect.ts: a service tag's interface).
///
/// Every method fails only with [`StoreError`]; domain outcomes are part of the success value
/// (`Option`, `Result`), so retries and the circuit breaker see store failures alone.
pub trait TodoRepository: Send + Sync + 'static {
  /// Every todo, by id.
  fn all(&self) -> Effect<Vec<Todo>, StoreError>;

  /// The todo `id`, if there is one.
  fn find(&self, id: TodoId) -> Effect<Option<Todo>, StoreError>;

  /// Stores a new, open todo titled `title`, or reports that the title is taken.
  fn add(&self, title: Title) -> Effect<Result<Todo, TitleTaken>, StoreError>;

  /// Stores every title or none: the first taken title (by a stored todo or an earlier title of
  /// the batch) is reported and nothing is stored.
  fn add_all(&self, titles: Vec<Title>) -> Effect<Result<Vec<Todo>, TitleTaken>, StoreError>;

  /// Replaces the stored state of `todo` (matched by id).
  fn save(&self, todo: Todo) -> Effect<Result<Todo, Or<TodoNotFound, TitleTaken>>, StoreError>;

  /// Removes the todo `id`; `false` when there was none.
  fn remove(&self, id: TodoId) -> Effect<bool, StoreError>;
}
