//! [`TodoStore`]: the state behind an [`InMemoryTodoRepository`](super::InMemoryTodoRepository).

use std::collections::BTreeMap;

use id_effect::Or;

use crate::domain::todo::{Title, TitleTaken, Todo, TodoId, TodoNotFound};

/// The todos by id and the last assigned id.
#[derive(Clone, Debug, Default)]
pub(crate) struct TodoStore {
  last_id: i64,
  todos: BTreeMap<TodoId, Todo>,
}

impl TodoStore {
  /// Every todo, by id.
  pub(crate) fn all(&self) -> Vec<Todo> {
    self.todos.values().cloned().collect()
  }

  /// The todo `id`.
  pub(crate) fn find(&self, id: TodoId) -> Option<Todo> {
    self.todos.get(&id).cloned()
  }

  /// Whether a todo other than `except` has `title`.
  fn taken(&self, title: &Title, except: Option<TodoId>) -> bool {
    self
      .todos
      .values()
      .any(|t| t.title() == title && Some(t.id()) != except)
  }

  /// Stores a new open todo with the next id.
  pub(crate) fn add(&mut self, title: Title) -> Result<Todo, TitleTaken> {
    if self.taken(&title, None) {
      return Err(TitleTaken(title));
    }
    self.last_id += 1;
    let todo = Todo::restore(TodoId::new(self.last_id), title, false);
    self.todos.insert(todo.id(), todo.clone());
    Ok(todo)
  }

  /// Stores every title, or none on the first taken one.
  pub(crate) fn add_all(&mut self, titles: Vec<Title>) -> Result<Vec<Todo>, TitleTaken> {
    let mut draft = self.clone();
    let todos = titles
      .into_iter()
      .map(|title| draft.add(title))
      .collect::<Result<Vec<_>, _>>()?;
    *self = draft;
    Ok(todos)
  }

  /// Replaces the stored state of `todo`.
  pub(crate) fn save(&mut self, todo: Todo) -> Result<Todo, Or<TodoNotFound, TitleTaken>> {
    if !self.todos.contains_key(&todo.id()) {
      return Err(Or::Left(TodoNotFound(todo.id())));
    }
    if self.taken(todo.title(), Some(todo.id())) {
      return Err(Or::Right(TitleTaken(todo.title().clone())));
    }
    self.todos.insert(todo.id(), todo.clone());
    Ok(todo)
  }

  /// Removes the todo `id`; whether there was one.
  pub(crate) fn remove(&mut self, id: TodoId) -> bool {
    self.todos.remove(&id).is_some()
  }
}
