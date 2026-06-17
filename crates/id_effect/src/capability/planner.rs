//! Shared topological planner for provider graphs.

use super::error::CapabilityPlannerError;
use crate::collections::EffectHashMap;
use crate::collections::hash_map;
use crate::collections::mutable_list::MutableList;
use std::collections::BTreeSet;

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

  let mut provider_by_cap = EffectHashMap::<String, String>::new();
  for node in nodes {
    if hash_map::has(&provider_by_cap, &node.provides) {
      let existing = hash_map::get(&provider_by_cap, &node.provides)
        .expect("key present")
        .clone();
      return Err(CapabilityPlannerError::ConflictingProvider {
        cap: node.provides.clone(),
        first: existing,
        second: node.id.clone(),
      });
    }
    provider_by_cap = hash_map::set(&provider_by_cap, node.provides.clone(), node.id.clone());
  }

  let mut indegree = EffectHashMap::<String, usize>::new();
  let mut edges = EffectHashMap::<String, Vec<String>>::new();
  for node in nodes {
    indegree = hash_map::modify(&indegree, node.id.clone(), |opt| Some(opt.unwrap_or(0)));
    edges = hash_map::modify(&edges, node.id.clone(), |opt| Some(opt.unwrap_or_default()));
  }

  for node in nodes {
    for required in &node.requires {
      let Some(provider) = hash_map::get(&provider_by_cap, required) else {
        return Err(CapabilityPlannerError::MissingProvider {
          provider: node.id.clone(),
          cap: required.clone(),
        });
      };
      let provider = provider.clone();
      if provider == node.id {
        continue;
      }
      edges = hash_map::modify(&edges, provider.clone(), |opt| {
        let mut v = opt.unwrap_or_default();
        v.push(node.id.clone());
        Some(v)
      });
      indegree = hash_map::modify(&indegree, node.id.clone(), |opt| Some(opt.unwrap_or(0) + 1));
    }
  }

  let queue = MutableList::<String>::make();
  for (id, &deg) in indegree.iter() {
    if deg == 0 {
      queue.append(id.clone());
    }
  }
  let order = MutableList::<String>::make();
  while let Some(next) = queue.shift() {
    order.append(next.clone());
    let dependents = hash_map::get(&edges, &next).cloned().unwrap_or_default();
    for dependent in dependents {
      indegree = hash_map::modify(&indegree, dependent.clone(), |opt| {
        let mut d = opt.expect("indegree exists");
        d -= 1;
        Some(d)
      });
      if hash_map::get(&indegree, &dependent) == Some(&0) {
        queue.append(dependent);
      }
    }
  }

  let order_vec = order.to_chunk().into_vec();
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
