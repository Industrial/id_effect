//! The in-memory store adapter.

mod in_memory_todo_repository;
mod in_memory_todo_repository_live;
mod todo_store;

pub use in_memory_todo_repository::InMemoryTodoRepository;
pub use in_memory_todo_repository_live::InMemoryTodoRepositoryLive;
pub(crate) use todo_store::TodoStore;
