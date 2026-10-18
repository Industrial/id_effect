//! Multi-projection rebuild planner: projections run in dependency order.

use crate::error::EventStoreError;
use crate::event_store::{EventStore, StoredEvent};
use crate::projection::{Projection, run_projection};
use id_effect::{Effect, fail};
use std::collections::{BTreeMap, BTreeSet, HashMap};

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

  /// Compute the execution order of the registered projections: every projection after the
  /// ones it depends on, ties broken by id (so the order is deterministic). A duplicate id, an
  /// unregistered dependency or a cycle is [`EventStoreError::Graph`].
  pub fn plan(&self) -> Result<Vec<String>, EventStoreError> {
    let mut pending: BTreeMap<&str, usize> = BTreeMap::new();
    for node in &self.nodes {
      if pending.insert(&node.id, 0).is_some() {
        return Err(EventStoreError::Graph(format!(
          "projection `{}` registered twice",
          node.id
        )));
      }
    }
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for node in &self.nodes {
      let deps: BTreeSet<&str> = node.depends_on.iter().map(String::as_str).collect();
      for dep in deps {
        if !pending.contains_key(dep) {
          return Err(EventStoreError::Graph(format!(
            "projection `{}` depends on unregistered `{dep}`",
            node.id
          )));
        }
        dependents.entry(dep).or_default().push(&node.id);
        *pending.entry(&node.id).or_default() += 1;
      }
    }
    let mut ready: BTreeSet<&str> = pending
      .iter()
      .filter(|(_, n)| **n == 0)
      .map(|(id, _)| *id)
      .collect();
    let mut order = Vec::with_capacity(self.nodes.len());
    while let Some(id) = ready.pop_first() {
      order.push(id.to_owned());
      for next in dependents.get(id).into_iter().flatten() {
        let n = pending.entry(next).or_default();
        *n -= 1;
        if *n == 0 {
          ready.insert(next);
        }
      }
    }
    if order.len() < self.nodes.len() {
      let stuck: Vec<&str> = pending
        .iter()
        .filter(|(_, n)| **n > 0)
        .map(|(id, _)| *id)
        .collect();
      return Err(EventStoreError::Graph(format!(
        "projection dependency cycle among {}",
        stuck.join(", ")
      )));
    }
    Ok(order)
  }

  /// Rebuild the projections in planned order from one read of `stream_id`. A planning failure
  /// or a planned id missing from `projections_by_id` is [`EventStoreError::Graph`].
  pub fn run_all<S, E, P, Store>(
    &self,
    store: &Store,
    stream_id: &str,
    from_version: u64,
    projections_by_id: HashMap<String, P>,
  ) -> Effect<Vec<(String, S)>, EventStoreError>
  where
    P: Projection<S, E> + Send + 'static,
    Store: EventStore<E> + ?Sized,
    E: Clone + Send + Sync + 'static,
    S: Send + 'static,
  {
    let order = match self.plan() {
      Ok(order) => order,
      Err(e) => return fail(e),
    };
    if let Some(id) = order.iter().find(|id| !projections_by_id.contains_key(*id)) {
      return fail(EventStoreError::Graph(format!(
        "projection `{id}` not registered in run_all map"
      )));
    }
    store
      .read(stream_id, from_version)
      .map(move |stored: Vec<StoredEvent<E>>| {
        let events: Vec<E> = stored.into_iter().map(|s| s.payload).collect();
        order
          .into_iter()
          .filter_map(|id| {
            let state = run_projection(projections_by_id.get(&id)?, events.iter().cloned());
            Some((id, state))
          })
          .collect()
      })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::edge::run_async;
  use crate::event_store::MemoryEventStore;
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
    runner.register(ProjectionNode::new("tail", ["derived"]));
    runner.register(ProjectionNode::new("derived", ["base"]));
    runner.register(ProjectionNode::new("base", Vec::<&str>::new()));
    let order = runner.plan().expect("plan");
    assert_eq!(order, ["base", "derived", "tail"]);
  }

  #[test]
  fn independent_projections_order_by_id() {
    let mut runner = ProjectionRunner::new();
    runner.register(ProjectionNode::new("b", Vec::<&str>::new()));
    runner.register(ProjectionNode::new("a", Vec::<&str>::new()));
    runner.register(ProjectionNode::new("c", ["a", "a"]));
    assert_eq!(runner.plan().expect("plan"), ["a", "b", "c"]);
  }

  #[test]
  fn cycle_fails_planning() {
    let mut runner = ProjectionRunner::new();
    runner.register(ProjectionNode::new("a", ["b"]));
    runner.register(ProjectionNode::new("b", ["a"]));
    let err = runner.plan().expect_err("cycle");
    assert_eq!(
      err,
      EventStoreError::Graph("projection dependency cycle among a, b".into())
    );
  }

  #[test]
  fn duplicate_and_missing_nodes_fail_planning() {
    let mut runner = ProjectionRunner::new();
    runner.register(ProjectionNode::new("a", Vec::<&str>::new()));
    runner.register(ProjectionNode::new("a", Vec::<&str>::new()));
    assert!(matches!(runner.plan(), Err(EventStoreError::Graph(m)) if m.contains("twice")));
    let mut runner = ProjectionRunner::new();
    runner.register(ProjectionNode::new("a", ["ghost"]));
    assert!(matches!(runner.plan(), Err(EventStoreError::Graph(m)) if m.contains("ghost")));
  }

  #[tokio::test]
  async fn run_all_rebuilds_in_order() {
    let store = MemoryEventStore::new();
    run_async(store.append("s", &[Evt::N(2), Evt::N(3)]))
      .await
      .expect("append");

    let mut runner = ProjectionRunner::new();
    runner.register(ProjectionNode::new("sum", Vec::<&str>::new()));
    let mut map = HashMap::new();
    map.insert("sum".to_string(), Sum);
    let out = run_async(runner.run_all(&store, "s", 1, map))
      .await
      .expect("run");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].1, 5);
  }

  #[tokio::test]
  async fn run_all_fails_on_unmapped_or_cyclic_projections() {
    let store = MemoryEventStore::<Evt>::new();
    let mut runner = ProjectionRunner::new();
    runner.register(ProjectionNode::new("sum", Vec::<&str>::new()));
    let err = run_async(runner.run_all(&store, "s", 1, HashMap::<String, Sum>::new()))
      .await
      .expect_err("unmapped");
    assert!(matches!(err, EventStoreError::Graph(m) if m.contains("run_all map")));
    runner.register(ProjectionNode::new("x", ["y"]));
    runner.register(ProjectionNode::new("y", ["x"]));
    let err = run_async(runner.run_all(&store, "s", 1, HashMap::<String, Sum>::new()))
      .await
      .expect_err("cycle");
    assert!(matches!(err, EventStoreError::Graph(_)));
  }
}
