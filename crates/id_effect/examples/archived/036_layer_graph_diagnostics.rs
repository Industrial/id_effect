//! Ex 036 — invalid provider graphs yield [`CapabilityPlannerError`] and diagnostics.

use id_effect::{CapabilityDiagnostic, CapabilityPlannerError, PlannerNode, plan_topological};

fn main() {
  let bad = vec![PlannerNode::new("x", ["Missing"], "X")];
  let err = plan_topological(&bad).unwrap_err();
  assert!(
    matches!(err, CapabilityPlannerError::MissingProvider { .. }),
    "{err:?}"
  );
  let diags: Vec<CapabilityDiagnostic> = vec![err.to_diagnostic()];
  assert!(!diags.is_empty());
  println!("036_layer_graph_diagnostics ok");
}
