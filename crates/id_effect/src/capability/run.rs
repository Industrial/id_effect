//! [`run`] / [`run_with`] — application entrypoints.

use super::env::Env;
use super::error::{CapabilityPlannerError, RunError};
use super::graph::CapabilityGraph;
use super::provider::{ProviderBox, ShutdownHook};
use super::set::FromEnv;
use crate::compute::ensure_default_fabric;
use crate::kernel::Effect;
use crate::runtime::run_blocking;
use std::sync::Arc;

/// Run a pure effect (no capabilities).
///
/// Installs a default [`crate::compute::ComputeFabric`] only if none is installed (see
/// [`ensure_default_fabric`]); drives `app` with [`run_blocking`] on the calling thread.
#[inline]
pub fn run<A, E>(app: Effect<A, E, ()>) -> Result<A, RunError<E>> {
  ensure_default_fabric();
  run_blocking(app, ()).map_err(RunError::Effect)
}

/// Run `app` after building capabilities from `providers`.
///
/// # Compute Fabric
///
/// If no fabric is installed, installs a default [`crate::compute::ComputeFabric`] (memory cap
/// 100%, max CPU) so adaptive parallelism and admission follow live telemetry. A fabric you
/// installed beforehand via [`crate::compute::install_fabric`] (e.g. with a custom
/// [`crate::compute::ResourcePolicy`]) is **kept**, never replaced. The default fabric samples
/// host telemetry lazily, so setup does not pay a CPU-sampling delay.
///
/// # Async effects
///
/// `run_with` is synchronous: it drives `app` with [`run_blocking`] on the calling thread — a
/// poll loop with a no-op waker and **no async executor or reactor**. Effects built from
/// `from_async` futures that complete by polling (or are already ready) work; effects that wait
/// on I/O, timers, or other wakeups (Tokio sockets, `tokio::time::sleep`, channels fed by other
/// tasks) will spin, stall, or panic for lack of a Tokio reactor. For those, build the env with
/// [`build_env`] and drive the effect with [`crate::run_async`] inside your executor, or use
/// `id_effect_tokio` (`TokioRuntime`, `spawn_blocking_run_async`). Do not call `run_with` from
/// inside an async task: it blocks the worker thread.
pub fn run_with<A, E, R, I>(providers: I, app: Effect<A, E, R>) -> Result<A, RunError<E>>
where
  I: IntoIterator<Item = ProviderBox>,
  R: FromEnv,
{
  let (env, hooks) = build_env_with_hooks(providers).map_err(RunError::Planner)?;
  R::verify(&env).map_err(RunError::Capability)?;
  let runtime = R::from_env(env);
  ensure_default_fabric();
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
