//! Proptest invariants for [`CapabilityGraph`] planning.

use id_effect::CapabilityPlannerError;
use id_effect::{PlannerNode, plan_topological};
use proptest::prelude::*;

proptest! {
  #[test]
  fn acyclic_plans_have_unique_order(nodes in prop::collection::vec((0u8..5u8, 0u8..5u8), 1..8)) {
    let mut caps = std::collections::BTreeSet::new();
    let planner_nodes: Vec<PlannerNode> = nodes
      .iter()
      .enumerate()
      .map(|(i, (req, prov))| {
        let id = format!("p{i}");
        let provides = format!("cap{prov}");
        caps.insert(prov);
        let requires = if caps.contains(req) && *req != *prov {
          vec![format!("cap{req}")]
        } else {
          vec![]
        };
        PlannerNode::new(id, requires, provides)
      })
      .collect();
    match plan_topological(&planner_nodes) {
      Ok(plan) => {
        let mut seen = std::collections::BTreeSet::new();
        for id in &plan.build_order {
          prop_assert!(seen.insert(id.clone()));
        }
        prop_assert_eq!(plan.build_order.len(), planner_nodes.len());
      }
      Err(CapabilityPlannerError::CycleDetected { .. })
      | Err(CapabilityPlannerError::MissingProvider { .. })
      | Err(CapabilityPlannerError::ConflictingProvider { .. })
      | Err(CapabilityPlannerError::DuplicateProviderId { .. }) => {}
    }
  }
}
