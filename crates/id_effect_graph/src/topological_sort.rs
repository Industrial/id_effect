//! Capability-style dependency resolution and topological ordering.

use crate::dag::sort_explicit;
use crate::error::GraphError;
use std::collections::{BTreeMap, BTreeSet};

/// One node in a dependency graph (mirrors capability planner nodes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyNode {
  /// Unique node id.
  pub id: String,
  /// Required capability names.
  pub requires: Vec<String>,
  /// Provided capability name.
  pub provides: String,
}

impl DependencyNode {
  /// New dependency node.
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

/// Plan topological order for `nodes`, resolving `requires` to providers via `provides`.
pub fn topological_sort(nodes: &[DependencyNode]) -> Result<Vec<String>, GraphError> {
  let mut ids = BTreeSet::new();
  for node in nodes {
    if !ids.insert(node.id.clone()) {
      return Err(GraphError::DuplicateNode {
        id: node.id.clone(),
      });
    }
  }

  let mut provider_by_cap = BTreeMap::<String, String>::new();
  for node in nodes {
    if let Some(existing) = provider_by_cap.insert(node.provides.clone(), node.id.clone()) {
      return Err(GraphError::ConflictingProvider {
        capability: node.provides.clone(),
        first: existing,
        second: node.id.clone(),
      });
    }
  }

  let node_ids: BTreeSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
  let mut edges = Vec::new();

  for node in nodes {
    for required in &node.requires {
      let Some(provider) = provider_by_cap.get(required) else {
        return Err(GraphError::MissingDependency {
          node: node.id.clone(),
          dependency: required.clone(),
        });
      };
      if provider != &node.id {
        edges.push((provider.clone(), node.id.clone()));
      }
    }
  }

  sort_explicit(&node_ids, &edges)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn acyclic_graph_orders_dependencies() {
    let nodes = vec![
      DependencyNode::new("db", Vec::<&str>::new(), "Db"),
      DependencyNode::new("repo", ["Db"], "Repo"),
    ];
    let order = topological_sort(&nodes).expect("plan");
    let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
    assert!(pos("db") < pos("repo"));
  }

  #[test]
  fn missing_provider_errors() {
    let nodes = vec![DependencyNode::new("repo", ["Db"], "Repo")];
    let err = topological_sort(&nodes).expect_err("missing");
    assert!(matches!(err, GraphError::MissingDependency { .. }));
  }

  #[test]
  fn duplicate_provider_id_errors() {
    let nodes = vec![
      DependencyNode::new("a", Vec::<&str>::new(), "A"),
      DependencyNode::new("a", Vec::<&str>::new(), "B"),
    ];
    let err = topological_sort(&nodes).unwrap_err();
    assert!(matches!(err, GraphError::DuplicateNode { .. }));
  }

  #[test]
  fn conflicting_providers_errors() {
    let nodes = vec![
      DependencyNode::new("a", Vec::<&str>::new(), "Cap"),
      DependencyNode::new("b", Vec::<&str>::new(), "Cap"),
    ];
    let err = topological_sort(&nodes).unwrap_err();
    assert!(matches!(err, GraphError::ConflictingProvider { .. }));
  }

  #[test]
  fn cycle_errors() {
    let nodes = vec![
      DependencyNode::new("a", ["B"], "A"),
      DependencyNode::new("b", ["A"], "B"),
    ];
    let err = topological_sort(&nodes).unwrap_err();
    assert!(matches!(err, GraphError::CycleDetected { .. }));
  }
}
