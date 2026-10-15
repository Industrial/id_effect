//! Shared run edge for the integration tests: the ambient tokio runtime (reqwest and
//! `tokio::process` need its IO driver), with the v1 `Result` shape.

#![allow(dead_code)]

use id_effect::Effect;
use id_effect::{Cause, Exit};
use id_effect_tokio::TokioRuntime;

/// Runs `eff` on the current tokio runtime; a typed failure is `Err`, a defect or interrupt panics.
pub async fn run<A, E>(eff: Effect<A, E>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + std::fmt::Debug + 'static,
{
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// The blocking edge outside any async context (pure and in-memory effects).
pub fn run_blocking<A, E>(eff: Effect<A, E>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + std::fmt::Debug + 'static,
{
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}
