//! The todo bounded context: the [`Todo`] aggregate, its value objects ([`TodoId`], [`Title`],
//! [`TodoChanges`]), its domain errors and the [`TodoRepository`] port.

mod invalid_title;
mod store_error;
mod title;
mod title_taken;
// One file per construct: the `Todo` aggregate lives in `todo.rs`.
#[allow(clippy::module_inception)]
mod todo;
mod todo_changes;
mod todo_id;
mod todo_not_found;
mod todo_repository;
mod todo_repository_service;

pub use invalid_title::InvalidTitle;
pub use store_error::StoreError;
pub use title::Title;
pub use title_taken::TitleTaken;
pub use todo::Todo;
pub use todo_changes::TodoChanges;
pub use todo_id::TodoId;
pub use todo_not_found::TodoNotFound;
pub use todo_repository::TodoRepository;
pub use todo_repository_service::TodoRepositoryService;
