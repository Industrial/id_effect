//! Ex 035 — `layer_graph!` declares a small planner graph.
use id_effect::layer_graph;

fn main() {
  let g = layer_graph! {
    a => [A];
    b : [A] => [B];
  };
  let plan = g.plan_topological().expect("plan");
  assert!(plan.build_order.contains(&"a".to_owned()));
  println!("035_layer_graph ok");
}
