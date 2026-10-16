//! Test doubles: [`TodoRepository`](crate::domain::todo::TodoRepository) wrappers over the
//! in-memory store that fail ([`FlakyTodoRepository`]) or answer late ([`SlowTodoRepository`]).

#[macro_use]
mod delegate_repository;
mod flaky_todo_repository;
mod slow_todo_repository;

pub(crate) use flaky_todo_repository::FlakyTodoRepository;
pub(crate) use slow_todo_repository::SlowTodoRepository;
