//! The application layer: one use case per endpoint, each an effect in the [`AppEnv`] that
//! validates its input into domain types and calls the store through the [`RequestPolicy`].
//! Every use case declares exactly the errors it can fail with (`err!(A | B | ..)`).

mod app_env;
mod create_todo;
mod create_todos;
mod delete_todo;
mod empty_batch;
mod get_todo;
mod list_todos;
mod new_app_env;
mod request_policy;
mod request_timeout;
mod store_failure;
mod update_todo;
#[cfg(test)]
mod use_case_harness;
mod with_store;

pub use app_env::AppEnv;
pub use create_todo::create_todo;
pub use create_todos::create_todos;
pub use delete_todo::delete_todo;
pub use empty_batch::EmptyBatch;
pub use get_todo::get_todo;
pub use list_todos::list_todos;
pub use new_app_env::new_app_env;
pub use request_policy::RequestPolicy;
pub use request_timeout::RequestTimeout;
pub use store_failure::StoreFailure;
pub use update_todo::update_todo;
#[cfg(test)]
pub(crate) use use_case_harness::UseCaseHarness;
pub(crate) use with_store::with_store;
