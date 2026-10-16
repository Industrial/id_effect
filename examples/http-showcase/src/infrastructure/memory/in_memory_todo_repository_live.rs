//! [`InMemoryTodoRepositoryLive`]: the in-memory store as a layer.

use id_effect::ProviderSpecDerive;

use super::InMemoryTodoRepository;
use crate::domain::todo::TodoRepositoryService;

/// Provides [`TodoRepositoryService`] over a fresh [`InMemoryTodoRepository`]
/// (`InMemoryTodoRepositoryLive::layer()`).
#[derive(ProviderSpecDerive)]
#[provides(
  TodoRepositoryService,
  with = || TodoRepositoryService::new(InMemoryTodoRepository::new())
)]
pub struct InMemoryTodoRepositoryLive;
