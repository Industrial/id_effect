//! The infrastructure layer: adapters for the domain's ports (in-memory and PostgreSQL stores),
//! configuration and telemetry.

pub mod config;
#[cfg(test)]
pub(crate) mod doubles;
pub mod memory;
pub mod postgres;
pub mod telemetry;
