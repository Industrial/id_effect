//! The wire shapes: request bodies (decoded through their `Schema`) and response bodies
//! (serialized with serde).

mod create_todo_request;
mod create_todos_request;
mod error_body;
mod todo_response;
mod update_todo_request;

pub use create_todo_request::CreateTodoRequest;
pub use create_todos_request::CreateTodosRequest;
pub use error_body::ErrorBody;
pub use todo_response::TodoResponse;
pub use update_todo_request::UpdateTodoRequest;
