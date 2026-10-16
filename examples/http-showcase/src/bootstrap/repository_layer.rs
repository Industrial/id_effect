//! [`repository_layer`]: the store the configuration asks for, as a layer.

use id_effect::{Layer, caps};

use super::StartupError;
use crate::domain::todo::TodoRepositoryService;
use crate::infrastructure::config::AppConfig;
use crate::infrastructure::memory::InMemoryTodoRepositoryLive;
use crate::infrastructure::postgres::{PgTodoRepositoryLive, database_layer};

/// [`TodoRepositoryService`] for `config`: with a database, the scoped PostgreSQL pool
/// ([`database_layer`]) feeding the migrated [`PgTodoRepositoryLive`] (the pool closes with the
/// owning scope); without one, a fresh [`InMemoryTodoRepositoryLive`].
pub fn repository_layer(config: &AppConfig) -> Layer<caps!(TodoRepositoryService), StartupError> {
  match &config.database {
    None => InMemoryTodoRepositoryLive::layer(),
    Some(db) => database_layer(db).provide_to(PgTodoRepositoryLive::layer()),
  }
}
