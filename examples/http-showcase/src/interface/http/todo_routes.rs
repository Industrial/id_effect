//! [`todo_routes`]: the `/todos` routes.

use axum::Router;
use axum::routing::{get, post};

use super::handlers::{
  create_todo_handler, create_todos_handler, delete_todo_handler, get_todo_handler,
  list_todos_handler, update_todo_handler,
};
use crate::application::AppEnv;

/// The `/todos` routes over the [`AppEnv`] state (the table: [`super`]).
pub fn todo_routes() -> Router<AppEnv> {
  Router::new()
    .route("/todos", get(list_todos_handler).post(create_todo_handler))
    .route("/todos/batch", post(create_todos_handler))
    .route(
      "/todos/{id}",
      get(get_todo_handler)
        .patch(update_todo_handler)
        .delete(delete_todo_handler),
    )
}
