//! [`run`] / [`run_with`] — application entrypoints.

use super::env::Env;
use super::error::{CapabilityPlannerError, RunError};
use super::graph::CapabilityGraph;
use super::provider::ProviderBox;
use crate::kernel::Effect;
use crate::runtime::run_blocking;

/// Run a pure effect (no capabilities).
#[inline]
pub fn run<A, E>(app: Effect<A, E, ()>) -> Result<A, RunError<E>> {
  run_blocking(app, ()).map_err(RunError::Effect)
}

/// Run `app` after building capabilities from `providers`.
pub fn run_with<A, E, I>(providers: I, app: Effect<A, E, Env>) -> Result<A, RunError<E>>
where
  I: IntoIterator<Item = ProviderBox>,
{
  let mut graph = CapabilityGraph::new();
  for p in providers {
    graph = graph.add(p.0);
  }
  let env = graph.build().map_err(RunError::Planner)?;
  run_blocking(app, env).map_err(RunError::Effect)
}

/// Build [`Env`] from providers without running an effect.
pub fn build_env<I>(providers: I) -> Result<Env, CapabilityPlannerError>
where
  I: IntoIterator<Item = ProviderBox>,
{
  let mut graph = CapabilityGraph::new();
  for p in providers {
    graph = graph.add(p.0);
  }
  graph.build()
}
