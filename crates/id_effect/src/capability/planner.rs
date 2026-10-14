//! Shared topological planner for provider graphs.

use super::error::CapabilityPlannerError;
use std::collections::{BTreeSet, HashMap, VecDeque};

/// One node in a provider dependency graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannerNode {
  /// Unique provider id.
  pub id: String,
  /// Required capability names.
  pub requires: Vec<String>,
  /// Provided capability name.
  pub provides: String,
}

impl PlannerNode {
  /// New node.
  pub fn new(
    id: impl Into<String>,
    requires: impl IntoIterator<Item = impl Into<String>>,
    provides: impl Into<String>,
  ) -> Self {
    Self {
      id: id.into(),
      requires: requires.into_iter().map(Into::into).collect(),
      provides: provides.into(),
    }
  }
}

/// Successful build order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannerPlan {
  /// Provider ids in dependency order.
  pub build_order: Vec<String>,
}

/// Plan topological order for `nodes`.
pub fn plan_topological(nodes: &[PlannerNode]) -> Result<PlannerPlan, CapabilityPlannerError> {
  let mut ids = BTreeSet::new();
  for node in nodes {
    if !ids.insert(node.id.clone()) {
      return Err(CapabilityPlannerError::DuplicateProviderId {
        id: node.id.clone(),
      });
    }
  }

  let mut provider_by_cap = HashMap::<String, String>::new();
  for node in nodes {
    if let Some(existing) = provider_by_cap.get(&node.provides) {
      return Err(CapabilityPlannerError::ConflictingProvider {
        cap: node.provides.clone(),
        first: existing.clone(),
        second: node.id.clone(),
      });
    }
    provider_by_cap.insert(node.provides.clone(), node.id.clone());
  }

  let mut indegree = HashMap::<String, usize>::new();
  let mut edges = HashMap::<String, Vec<String>>::new();
  for node in nodes {
    indegree.entry(node.id.clone()).or_insert(0);
    edges.entry(node.id.clone()).or_default();
  }

  for node in nodes {
    for required in &node.requires {
      let Some(provider) = provider_by_cap.get(required) else {
        return Err(CapabilityPlannerError::MissingProvider {
          provider: node.id.clone(),
          cap: required.clone(),
        });
      };
      let provider = provider.clone();
      if provider == node.id {
        continue;
      }
      edges.entry(provider).or_default().push(node.id.clone());
      *indegree.entry(node.id.clone()).or_insert(0) += 1;
    }
  }

  let mut queue = VecDeque::<String>::new();
  for (id, &deg) in indegree.iter() {
    if deg == 0 {
      queue.push_back(id.clone());
    }
  }
  let mut order_vec = Vec::<String>::with_capacity(nodes.len());
  while let Some(next) = queue.pop_front() {
    let dependents = edges.get(&next).cloned().unwrap_or_default();
    order_vec.push(next);
    for dependent in dependents {
      let d = indegree.get_mut(&dependent).expect("indegree exists");
      *d -= 1;
      if *d == 0 {
        queue.push_back(dependent);
      }
    }
  }

  if order_vec.len() != nodes.len() {
    let cycle_nodes = indegree
      .iter()
      .filter_map(|(id, &deg)| if deg > 0 { Some(id.clone()) } else { None })
      .collect::<Vec<_>>();
    return Err(CapabilityPlannerError::CycleDetected { nodes: cycle_nodes });
  }

  Ok(PlannerPlan {
    build_order: order_vec,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn acyclic_graph_orders_dependencies() {
    let nodes = vec![
      PlannerNode::new("db", Vec::<&str>::new(), "Db"),
      PlannerNode::new("repo", ["Db"], "Repo"),
    ];
    let plan = plan_topological(&nodes).expect("plan");
    let pos = |id: &str| plan.build_order.iter().position(|x| x == id).unwrap();
    assert!(pos("db") < pos("repo"));
  }

  #[test]
  fn missing_provider_errors() {
    let nodes = vec![PlannerNode::new("repo", ["Db"], "Repo")];
    let err = plan_topological(&nodes).expect_err("missing");
    assert!(matches!(
      err,
      CapabilityPlannerError::MissingProvider { .. }
    ));
  }
}

#[cfg(test)]
mod planner_tests {
  use super::*;

  #[test]
  fn duplicate_provider_id_in_topological_plan() {
    let nodes = vec![
      PlannerNode::new("a", Vec::<&str>::new(), "A"),
      PlannerNode::new("a", Vec::<&str>::new(), "B"),
    ];
    let err = plan_topological(&nodes).unwrap_err();
    assert!(matches!(
      err,
      CapabilityPlannerError::DuplicateProviderId { .. }
    ));
  }

  #[test]
  fn conflicting_providers_in_topological_plan() {
    let nodes = vec![
      PlannerNode::new("a", Vec::<&str>::new(), "Cap"),
      PlannerNode::new("b", Vec::<&str>::new(), "Cap"),
    ];
    let err = plan_topological(&nodes).unwrap_err();
    assert!(matches!(
      err,
      CapabilityPlannerError::ConflictingProvider { .. }
    ));
  }

  #[test]
  fn missing_dependency_in_topological_plan() {
    let nodes = vec![PlannerNode::new("db", ["Config"], "Database")];
    let err = plan_topological(&nodes).unwrap_err();
    assert!(matches!(
      err,
      CapabilityPlannerError::MissingProvider { .. }
    ));
  }

  #[test]
  fn cycle_in_topological_plan() {
    let nodes = vec![
      PlannerNode::new("a", ["B"], "A"),
      PlannerNode::new("b", ["A"], "B"),
    ];
    let err = plan_topological(&nodes).unwrap_err();
    assert!(matches!(err, CapabilityPlannerError::CycleDetected { .. }));
  }
}
