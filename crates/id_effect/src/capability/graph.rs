//! [`CapabilityGraph`] — typed provider graph planning.

use super::error::{CapabilityDiagnostic, CapabilityPlannerError, ProviderError};
use super::id::CapabilityId;
use super::planner::{PlannerNode, plan_topological};
use super::provider::ProviderNode;
use crate::collections::EffectHashMap;
use crate::collections::hash_map;
use crate::runtime::run_blocking;
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
    self.check_provider_conflicts()?;

    let mut cap_name_by_id = EffectHashMap::<CapabilityId, String>::new();
    for node in &self.nodes {
      cap_name_by_id = hash_map::set(
        &cap_name_by_id,
        node.provides(),
        planner_cap_key(node.provides(), node.cap_name()),
      );
    }

    let provided_caps: std::collections::HashSet<CapabilityId> =
      self.nodes.iter().map(|n| n.provides()).collect();

    let planner_nodes: Vec<PlannerNode> = self
      .nodes
      .iter()
      .map(|n| {
        let optional: std::collections::HashSet<CapabilityId> =
          n.optional_requires().iter().copied().collect();
        let requires = n
          .requires()
          .iter()
          .filter(|cap_id| provided_caps.contains(cap_id) || !optional.contains(cap_id))
          .map(|cap_id| {
            hash_map::get(&cap_name_by_id, cap_id)
              .cloned()
              .unwrap_or_else(|| planner_cap_key(*cap_id, &format!("{cap_id:?}")))
          })
          .collect::<Vec<_>>();
        PlannerNode::new(
          n.id(),
          requires,
          planner_cap_key(n.provides(), n.cap_name()),
        )
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
    self.build_from(super::Env::new())
  }

  /// Build providers on top of an existing environment (scoped child).
  pub fn build_from(&self, mut env: super::Env) -> Result<super::Env, CapabilityPlannerError> {
    let order = self.plan()?;
    for idx in order {
      let node = &self.nodes[idx];
      env = if node.uses_effectful_build() {
        run_blocking(node.build_effect(&env), env).map_err(map_provider_err(node.id()))?
      } else {
        node.build(&env).map_err(map_provider_err(node.id()))?
      };
      if let Some(interval) = node.refresh_interval()
        && interval.as_nanos() > 0
      {
        let _interval = interval;
      }
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

  fn check_provider_conflicts(&self) -> Result<(), CapabilityPlannerError> {
    let mut provider_by_cap = EffectHashMap::<CapabilityId, (String, String)>::new();
    for node in &self.nodes {
      let cap = node.provides();
      if let Some((first_id, first_name)) = hash_map::get(&provider_by_cap, &cap) {
        return Err(CapabilityPlannerError::ConflictingProvider {
          cap: first_name.clone(),
          first: first_id.clone(),
          second: node.id().to_string(),
        });
      }
      provider_by_cap = hash_map::set(
        &provider_by_cap,
        cap,
        (node.id().to_string(), node.cap_name().to_string()),
      );
    }
    Ok(())
  }
}

fn planner_cap_key(cap: CapabilityId, name: &str) -> String {
  match cap.variant() {
    Some(v) => format!("{name}:{v}"),
    None => name.to_string(),
  }
}

fn map_provider_err(provider: &str) -> impl Fn(ProviderError) -> CapabilityPlannerError + '_ {
  move |e| CapabilityPlannerError::MissingProvider {
    provider: provider.to_string(),
    cap: e.message,
  }
}

#[cfg(test)]
#[allow(dead_code, clippy::new_ret_no_self)]
mod graph_feature_tests {
  use super::*;
  use crate::capability::Env;
  use crate::capability::error::ProviderError;
  use crate::capability::key::CapabilityKey;
  use crate::capability::provider::ProviderSpec;
  use crate::provide;

  #[::id_effect::capability(u32)]
  struct OptionalCfg;
  #[::id_effect::capability(u32)]
  struct Db;

  struct DbWithOptional;
  impl ProviderSpec for DbWithOptional {
    type Key = DbKey;
    type Output = u32;
    fn provider_id() -> &'static str {
      "db"
    }
    fn requires() -> &'static [CapabilityId] {
      static R: std::sync::LazyLock<Vec<CapabilityId>> =
        std::sync::LazyLock::new(|| vec![OptionalCfgKey::id()]);
      R.as_slice()
    }
    fn optional_requires() -> &'static [CapabilityId] {
      static O: std::sync::LazyLock<Vec<CapabilityId>> =
        std::sync::LazyLock::new(|| vec![OptionalCfgKey::id()]);
      O.as_slice()
    }
    fn provide(_: &Env) -> Result<u32, ProviderError> {
      Ok(9)
    }
  }

  #[test]
  fn optional_dep_absent_plans() {
    let g = CapabilityGraph::new().add(provide!(DbWithOptional).0);
    assert!(g.plan().is_ok());
  }

  #[test]
  fn shared_provider_builds() {
    struct SharedDb;
    impl ProviderSpec for SharedDb {
      type Key = DbKey;
      type Output = u32;
      fn provider_id() -> &'static str {
        "shared-db"
      }
      fn shared() -> bool {
        true
      }
      fn provide(_: &Env) -> Result<u32, ProviderError> {
        Ok(42)
      }
    }
    let g = CapabilityGraph::new().add(provide!(SharedDb).0);
    let env = g.build().expect("build");
    assert_eq!(*env.get::<DbKey>(), 42);
  }

  #[test]
  fn conflicting_providers_error() {
    #[derive(::id_effect::ProviderSpecDerive)]
    #[provides(DbKey)]
    struct DbA;
    impl DbA {
      fn new() -> u32 {
        1
      }
    }
    #[derive(::id_effect::ProviderSpecDerive)]
    #[provides(DbKey)]
    struct DbB;
    impl DbB {
      fn new() -> u32 {
        2
      }
    }
    let g = CapabilityGraph::new()
      .add(provide!(DbA).0)
      .add(provide!(DbB).0);
    let err = g.plan().expect_err("conflict");
    assert!(matches!(
      err,
      CapabilityPlannerError::ConflictingProvider { .. }
    ));
  }
  #[test]
  fn build_runs_refresh_interval_branch() {
    struct RefreshDb;
    impl ProviderSpec for RefreshDb {
      type Key = DbKey;
      type Output = u32;
      fn provider_id() -> &'static str {
        "refresh-db"
      }
      fn refresh_interval() -> Option<std::time::Duration> {
        Some(std::time::Duration::from_millis(1))
      }
      fn provide(_: &Env) -> Result<u32, ProviderError> {
        Ok(99)
      }
    }
    let env = CapabilityGraph::new()
      .add(provide!(RefreshDb).0)
      .build()
      .expect("build");
    assert_eq!(*env.get::<DbKey>(), 99);
  }

  #[test]
  fn build_from_parent_env() {
    let parent = Env::new();
    #[derive(::id_effect::ProviderSpecDerive)]
    #[provides(DbKey)]
    struct DbLive;
    impl DbLive {
      fn new() -> u32 {
        5
      }
    }
    let g = CapabilityGraph::new().add(provide!(DbLive).0);
    let child = g.build_from(parent).expect("build_from");
    assert_eq!(*child.get::<DbKey>(), 5);
  }

  #[test]
  fn diagnostics_reports_missing_required() {
    struct NeedsCfg;
    impl ProviderSpec for NeedsCfg {
      type Key = DbKey;
      type Output = u32;
      fn provider_id() -> &'static str {
        "db-needs-cfg"
      }
      fn requires() -> &'static [CapabilityId] {
        static R: std::sync::LazyLock<Vec<CapabilityId>> =
          std::sync::LazyLock::new(|| vec![OptionalCfgKey::id()]);
        R.as_slice()
      }
      fn provide(_: &Env) -> Result<u32, ProviderError> {
        Ok(1)
      }
    }
    let g = CapabilityGraph::new().add(provide!(NeedsCfg).0);
    let diags = g.diagnostics();
    assert!(!diags.is_empty() || g.plan().is_err());
  }
}
