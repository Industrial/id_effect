//! The `/todos` handlers: decode the request, run its use case as one request fiber
//! ([`id_effect_axum::execute`]) and answer the outcome (failures through
//! [`IntoApiError`](super::IntoApiError)).

mod create_todo_handler;
mod create_todos_handler;
mod delete_todo_handler;
mod get_todo_handler;
mod list_todos_handler;
mod update_todo_handler;

pub use create_todo_handler::create_todo_handler;
pub use create_todos_handler::create_todos_handler;
pub use delete_todo_handler::delete_todo_handler;
pub use get_todo_handler::get_todo_handler;
pub use list_todos_handler::list_todos_handler;
pub use update_todo_handler::update_todo_handler;
