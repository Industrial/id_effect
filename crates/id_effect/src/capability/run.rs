//! [`run`] / [`run_with`] — application entrypoints.

use super::env::Env;
use super::error::{CapabilityPlannerError, RunError};
use super::graph::CapabilityGraph;
use super::provider::{ProviderBox, ShutdownHook};
use super::set::FromEnv;
use crate::compute::{ComputeFabric, install_fabric};
use crate::kernel::Effect;
use crate::runtime::run_blocking;
use std::sync::Arc;

/// Run a pure effect (no capabilities).
#[inline]
pub fn run<A, E>(app: Effect<A, E, ()>) -> Result<A, RunError<E>> {
  run_blocking(app, ()).map_err(RunError::Effect)
}

/// Run `app` after building capabilities from `providers`.
///
/// Installs a default [`ComputeFabric`] (memory cap 100%, max CPU) for the duration of the run
/// so adaptive parallelism and admission follow live telemetry. Provide an explicit fabric via
/// [`crate::compute::install_fabric`] before calling when you need a custom [`crate::compute::ResourcePolicy`].
pub fn run_with<A, E, R, I>(providers: I, app: Effect<A, E, R>) -> Result<A, RunError<E>>
where
  I: IntoIterator<Item = ProviderBox>,
  R: FromEnv,
{
  let (env, hooks) = build_env_with_hooks(providers).map_err(RunError::Planner)?;
  R::verify(&env).map_err(RunError::Capability)?;
  let runtime = R::from_env(env);
  let fabric = Arc::new(ComputeFabric::memory_cap_max_cpu(1.0));
  install_fabric(Arc::clone(&fabric));
  let result = run_blocking(app, runtime).map_err(RunError::Effect);
  for hook in hooks.into_iter().rev() {
    hook.shutdown();
  }
  result
}

/// Build [`Env`] from providers without running an effect.
pub fn build_env<I>(providers: I) -> Result<Env, CapabilityPlannerError>
where
  I: IntoIterator<Item = ProviderBox>,
{
  build_env_with_hooks(providers).map(|(env, _)| env)
}

fn build_env_with_hooks<I>(
  providers: I,
) -> Result<(Env, Vec<Arc<dyn ShutdownHook>>), CapabilityPlannerError>
where
  I: IntoIterator<Item = ProviderBox>,
{
  let mut graph = CapabilityGraph::new();
  let mut hooks = Vec::new();
  for p in providers {
    if let Some(h) = p.0.shutdown_hook() {
      hooks.push(h);
    }
    graph = graph.add(p.0);
  }
  let env = graph.build()?;
  Ok((env, hooks))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::Cap;
  use crate::provide;
  use crate::{Effect, caps, effect, run_with};

  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  #[allow(dead_code)]
  struct RunCap(pub u32);

  #[derive(::id_effect::ProviderSpecDerive)]
  #[provides(RunCap)]
  struct RunCapLive;

  impl RunCapLive {
    #[allow(clippy::new_ret_no_self)]
    fn new() -> RunCap {
      RunCap(7)
    }
  }

  #[test]
  fn build_env_materializes_capability_env() {
    let env = build_env([provide!(RunCapLive)]).expect("env");
    assert_eq!(env.get::<Cap<RunCap>>().0, 7);
  }

  #[test]
  fn run_with_executes_and_shuts_down() {
    let app: Effect<u32, (), caps!(RunCap)> = effect!(|r| {
      let cap = ~RunCap;
      cap.0
    });
    assert_eq!(run_with([provide!(RunCapLive)], app).expect("run"), 7);
  }
}
