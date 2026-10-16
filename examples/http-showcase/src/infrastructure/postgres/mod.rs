//! The PostgreSQL store adapter: [`PgTodoRepository`] implements the
//! [`TodoRepository`](crate::domain::todo::TodoRepository) port on the `SqlClientService`
//! capability.
//!
//! The wiring, outermost first:
//!
//! - [`database_layer`] opens the scoped pool of a
//!   [`DatabaseConfig`](crate::infrastructure::config::DatabaseConfig) (`PgSqlEnv`);
//! - [`PgTodoRepositoryLive::layer`] applies the [`Migration`] on it and provides
//!   `TodoRepositoryService`.
//!
//! The schema is `migrations/0001_init.sql`. SQL failures surface as
//! [`StoreError`](crate::domain::todo::StoreError) (`impl From<SqlError> for StoreError`);
//! a taken title is a domain outcome, recognised from PostgreSQL's unique-violation message.

mod batch_abort;
mod database_layer;
#[cfg(test)]
mod live_pg;
mod migration;
mod pg_todo_repository;
mod pg_todo_repository_live;
mod store_error_from_sql;
mod todo_row;
mod unique_violation;

pub(crate) use batch_abort::BatchAbort;
pub use database_layer::database_layer;
#[cfg(test)]
pub(crate) use live_pg::LivePg;
pub use migration::Migration;
pub use pg_todo_repository::PgTodoRepository;
pub use pg_todo_repository_live::PgTodoRepositoryLive;
pub(crate) use todo_row::TodoRow;
pub(crate) use unique_violation::UniqueViolation;
