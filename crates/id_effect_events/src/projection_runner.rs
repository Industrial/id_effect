//! Multi-projection rebuild planner using [`id_effect_graph`].

use crate::error::EventStoreError;
use crate::event_store::{EventStore, StoredEvent};
use crate::projection::{Projection, run_projection};
use id_effect::run_async;
use id_effect_graph::{DependencyNode, topological_sort};
use std::collections::HashMap;

/// One projection node in a dependency graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionNode {
  /// Unique projection id.
  pub id: String,
  /// Projection ids that must run before this one.
  pub depends_on: Vec<String>,
}

impl ProjectionNode {
  /// New projection node.
  pub fn new(
    id: impl Into<String>,
    depends_on: impl IntoIterator<Item = impl Into<String>>,
  ) -> Self {
    Self {
      id: id.into(),
      depends_on: depends_on.into_iter().map(Into::into).collect(),
    }
  }
}

/// Plans topological order for registered projection nodes.
#[derive(Default)]
pub struct ProjectionRunner {
  nodes: Vec<ProjectionNode>,
}

impl ProjectionRunner {
  /// Empty runner.
  pub fn new() -> Self {
    Self { nodes: Vec::new() }
  }

  /// Register a projection node (metadata only; pass implementations to [`Self::run_all`]).
  pub fn register(&mut self, node: ProjectionNode) {
    self.nodes.push(node);
  }

  /// Compute topological execution order for registered projections.
  pub fn plan(&self) -> Result<Vec<String>, EventStoreError> {
    let graph_nodes: Vec<DependencyNode> = self
      .nodes
      .iter()
      .map(|node| DependencyNode::new(node.id.clone(), node.depends_on.clone(), node.id.clone()))
      .collect();
    topological_sort(&graph_nodes).map_err(|e| EventStoreError::Graph(e.to_string()))
  }

  /// Rebuild projections in planned order using `projections_by_id`.
  pub async fn run_all<S, E, P, Store>(
    &self,
    store: &Store,
    stream_id: &str,
    from_version: u64,
    projections_by_id: &HashMap<String, P>,
  ) -> Result<Vec<(String, S)>, EventStoreError>
  where
    P: Projection<S, E>,
    Store: EventStore<E>,
    E: Clone + Send + Sync + 'static,
    S: Send + 'static,
  {
    let order = self.plan()?;
    let stored: Vec<StoredEvent<E>> = run_async(store.read(stream_id, from_version), ()).await?;
    let events: Vec<E> = stored.into_iter().map(|s| s.payload).collect();

    let mut results = Vec::with_capacity(order.len());
    for id in order {
      let projection = projections_by_id.get(&id).ok_or_else(|| {
        EventStoreError::Graph(format!("projection `{id}` not registered in run_all map"))
      })?;
      let state = run_projection(projection, events.iter().cloned());
      results.push((id, state));
    }
    Ok(results)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::event_store::MemoryEventStore;
  use id_effect::run_blocking;
  use std::collections::HashMap;

  #[derive(Clone, Debug, PartialEq, Eq)]
  enum Evt {
    N(u32),
  }

  struct Sum;

  impl Projection<u32, Evt> for Sum {
    fn initial(&self) -> u32 {
      0
    }
    fn apply(&self, state: u32, event: &Evt) -> u32 {
      match event {
        Evt::N(n) => state + n,
      }
    }
  }

  #[test]
  fn three_projection_dag_orders_dependencies() {
    let mut runner = ProjectionRunner::new();
    runner.register(ProjectionNode::new("base", Vec::<&str>::new()));
    runner.register(ProjectionNode::new("derived", ["base"]));
    runner.register(ProjectionNode::new("tail", ["derived"]));
    let order = runner.plan().expect("plan");
    let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
    assert!(pos("base") < pos("derived"));
    assert!(pos("derived") < pos("tail"));
  }

  #[test]
  fn cycle_fails_planning() {
    let mut runner = ProjectionRunner::new();
    runner.register(ProjectionNode::new("a", ["b"]));
    runner.register(ProjectionNode::new("b", ["a"]));
    let err = runner.plan().expect_err("cycle");
    assert!(matches!(err, EventStoreError::Graph(_)));
  }

  #[tokio::test]
  async fn run_all_rebuilds_in_order() {
    let store = MemoryEventStore::new();
    run_blocking(store.append("s", &[Evt::N(2), Evt::N(3)]), ()).expect("append");

    let mut runner = ProjectionRunner::new();
    runner.register(ProjectionNode::new("sum", Vec::<&str>::new()));
    let mut map = HashMap::new();
    map.insert("sum".to_string(), Sum);
    let out = runner.run_all(&store, "s", 1, &map).await.expect("run");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].1, 5);
  }
}
