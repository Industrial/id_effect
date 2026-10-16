//! The HTTP interface: decode the request (path id, JSON body through its `Schema`), run the
//! [`application`](crate::application) use case as one request fiber
//! ([`id_effect_axum::execute`]) and answer its outcome (failures: [`IntoApiError`]).
//!
//! | Method and path | Body | Success |
//! |---|---|---|
//! | `GET /todos` | | `200` `[Todo]` |
//! | `POST /todos` | `{"title": ".."}` | `201` `Todo` |
//! | `POST /todos/batch` | `{"titles": [".."]}` | `201` `[Todo]`, all or none |
//! | `GET /todos/{id}` | | `200` `Todo` |
//! | `PATCH /todos/{id}` | `{"title"?: "..", "completed"?: bool}` | `200` `Todo` |
//! | `DELETE /todos/{id}` | | `204` |
//!
//! A `Todo` is `{"id": 1, "title": "..", "completed": false}` ([`TodoResponse`]). A malformed id
//! or body is `422`. Every error answers `{"error": "<code>", "message": "<text>"}`
//! ([`ErrorBody`]).

mod api_error;
mod decode;
mod dto;
mod handlers;
mod into_api_error;
mod router;
mod todo_routes;

pub use api_error::ApiError;
pub use dto::{CreateTodoRequest, CreateTodosRequest, ErrorBody, TodoResponse, UpdateTodoRequest};
pub use into_api_error::IntoApiError;
pub use router::router;
pub use todo_routes::todo_routes;
