//! Ex 036 — Invalid graphs yield `LayerPlannerError` and diagnostics.
use id_effect::{LayerDiagnostic, LayerPlannerError, layer_graph};

fn main() {
  let bad = layer_graph! {
    x : [Missing] => [X];
  };
  let err = bad.plan_topological().unwrap_err();
  assert!(
    matches!(err, LayerPlannerError::MissingProvider { .. }),
    "{err:?}"
  );
  let diags: Vec<LayerDiagnostic> = bad.diagnostics();
  assert!(!diags.is_empty());
  println!("036_layer_graph_diagnostics ok");
}
