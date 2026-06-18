//! [`Dag`] — explicit directed graph with topological sorting.

use crate::error::GraphError;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Directed graph keyed by node id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dag {
  nodes: BTreeSet<String>,
  /// `from -> to` means `to` depends on `from`.
  edges: Vec<(String, String)>,
}

impl Dag {
  /// Empty graph.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Register a node id.
  pub fn add_node(&mut self, id: impl Into<String>) -> Result<(), GraphError> {
    let id = id.into();
    if !self.nodes.insert(id.clone()) {
      return Err(GraphError::DuplicateNode { id });
    }
    Ok(())
  }

  /// Register a dependency edge (`dependency` must be built before `dependent`).
  pub fn add_edge(
    &mut self,
    dependency: impl Into<String>,
    dependent: impl Into<String>,
  ) -> Result<(), GraphError> {
    let dependency = dependency.into();
    let dependent = dependent.into();
    if !self.nodes.contains(&dependency) {
      return Err(GraphError::UnknownNode { id: dependency });
    }
    if !self.nodes.contains(&dependent) {
      return Err(GraphError::UnknownNode { id: dependent });
    }
    if dependency != dependent {
      self.edges.push((dependency, dependent));
    }
    Ok(())
  }

  /// Node ids in topological order (dependencies first).
  pub fn sort(&self) -> Result<Vec<String>, GraphError> {
    sort_explicit(&self.nodes, &self.edges)
  }

  /// Registered node count.
  #[inline]
  pub fn node_count(&self) -> usize {
    self.nodes.len()
  }

  /// Edge count.
  #[inline]
  pub fn edge_count(&self) -> usize {
    self.edges.len()
  }
}

pub(crate) fn sort_explicit(
  nodes: &BTreeSet<String>,
  edges: &[(String, String)],
) -> Result<Vec<String>, GraphError> {
  let mut indegree: BTreeMap<String, usize> = nodes.iter().map(|id| (id.clone(), 0)).collect();
  let mut adjacency: BTreeMap<String, Vec<String>> =
    nodes.iter().map(|id| (id.clone(), Vec::new())).collect();

  for (from, to) in edges {
    adjacency
      .get_mut(from)
      .expect("from exists")
      .push(to.clone());
    *indegree.get_mut(to).expect("to exists") += 1;
  }

  let mut queue: VecDeque<String> = indegree
    .iter()
    .filter_map(|(id, &deg)| if deg == 0 { Some(id.clone()) } else { None })
    .collect();

  let mut order = Vec::with_capacity(nodes.len());
  while let Some(next) = queue.pop_front() {
    order.push(next.clone());
    let dependents = adjacency.get(&next).cloned().unwrap_or_default();
    for dependent in dependents {
      let entry = indegree.get_mut(&dependent).expect("dependent exists");
      *entry -= 1;
      if *entry == 0 {
        queue.push_back(dependent);
      }
    }
  }

  if order.len() != nodes.len() {
    let cycle_nodes = indegree
      .into_iter()
      .filter_map(|(id, deg)| if deg > 0 { Some(id) } else { None })
      .collect();
    return Err(GraphError::CycleDetected { nodes: cycle_nodes });
  }

  Ok(order)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn explicit_dag_orders_dependencies() {
    let mut dag = Dag::new();
    dag.add_node("db").unwrap();
    dag.add_node("repo").unwrap();
    dag.add_edge("db", "repo").unwrap();
    let order = dag.sort().unwrap();
    let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
    assert!(pos("db") < pos("repo"));
  }

  #[test]
  fn cycle_is_reported() {
    let mut dag = Dag::new();
    dag.add_node("a").unwrap();
    dag.add_node("b").unwrap();
    dag.add_edge("a", "b").unwrap();
    dag.add_edge("b", "a").unwrap();
    assert!(matches!(dag.sort(), Err(GraphError::CycleDetected { .. })));
  }

  #[test]
  fn unknown_edge_endpoint_errors() {
    let mut dag = Dag::new();
    dag.add_node("a").unwrap();
    let err = dag.add_edge("a", "missing").unwrap_err();
    assert!(matches!(err, GraphError::UnknownNode { .. }));
  }
}
