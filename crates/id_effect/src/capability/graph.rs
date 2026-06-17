//! [`CapabilityGraph`] — typed provider graph planning.

use super::error::{CapabilityDiagnostic, CapabilityPlannerError, ProviderError};
use super::id::CapabilityId;
use super::planner::{PlannerNode, plan_topological};
use super::provider::ProviderNode;
use crate::collections::EffectHashMap;
use crate::collections::hash_map;
use std::fmt;
use std::sync::Arc;

/// Graph of providers for automatic build ordering.
#[derive(Default)]
pub struct CapabilityGraph {
  nodes: Vec<Arc<dyn ProviderNode>>,
}

impl fmt::Debug for CapabilityGraph {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CapabilityGraph")
      .field("nodes", &self.nodes.len())
      .finish()
  }
}

impl CapabilityGraph {
  /// Empty graph.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Add a provider node.
  pub fn add(mut self, node: Arc<dyn ProviderNode>) -> Self {
    self.nodes.push(node);
    self
  }

  /// Plan build order or return first planner error.
  pub fn plan(&self) -> Result<Vec<usize>, CapabilityPlannerError> {
    let mut cap_name_by_id = EffectHashMap::<CapabilityId, String>::new();
    for node in &self.nodes {
      cap_name_by_id = hash_map::set(
        &cap_name_by_id,
        node.provides(),
        node.cap_name().to_string(),
      );
    }

    let planner_nodes: Vec<PlannerNode> = self
      .nodes
      .iter()
      .map(|n| {
        let requires = n
          .requires()
          .iter()
          .map(|cap_id| {
            hash_map::get(&cap_name_by_id, cap_id)
              .cloned()
              .unwrap_or_else(|| format!("{cap_id:?}"))
          })
          .collect::<Vec<_>>();
        PlannerNode::new(n.id(), requires, n.cap_name())
      })
      .collect();
    let plan = plan_topological(&planner_nodes)?;
    let mut indices = Vec::with_capacity(plan.build_order.len());
    for id in &plan.build_order {
      let idx = self
        .nodes
        .iter()
        .position(|n| n.id() == id)
        .expect("planner id must exist");
      indices.push(idx);
    }
    Ok(indices)
  }

  /// Build environment by executing providers in planned order.
  pub fn build(&self) -> Result<super::Env, CapabilityPlannerError> {
    let order = self.plan()?;
    let mut env = super::Env::new();
    for idx in order {
      let node = &self.nodes[idx];
      env = node.build(&env).map_err(map_provider_err(node.id()))?;
    }
    Ok(env)
  }

  /// Diagnostics (empty if plan succeeds).
  pub fn diagnostics(&self) -> Vec<CapabilityDiagnostic> {
    match self.plan() {
      Ok(_) => Vec::new(),
      Err(e) => vec![e.to_diagnostic()],
    }
  }
}

fn map_provider_err(provider: &str) -> impl Fn(ProviderError) -> CapabilityPlannerError + '_ {
  move |e| CapabilityPlannerError::MissingProvider {
    provider: provider.to_string(),
    cap: e.message,
  }
}
