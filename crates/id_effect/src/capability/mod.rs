//! Capability-first dependency injection (v2).
//!
//! Traits + [`Env`] + [`CapabilityGraph`] + [`run_with`](run::run_with).

mod env;
mod error;
mod graph;
mod id;
mod key;
mod needs;
mod planner;
mod provider;
mod run;
mod set;

pub use env::{Caps, Env};
pub use error::{
  CapabilityDiagnostic, CapabilityError, CapabilityPlannerError, ProviderError, RunError,
};
pub use graph::CapabilityGraph;
pub use id::CapabilityId;
pub use key::{Capability, CapabilityKey};
pub use needs::Needs;
pub use planner::{PlannerNode, PlannerPlan, plan_topological};
pub use provider::{Provider, ProviderBox, ProviderNode, ProviderSpec};
pub use run::{build_env, run, run_with};
pub use set::{CapabilitySet, HasCap, NoCaps};
