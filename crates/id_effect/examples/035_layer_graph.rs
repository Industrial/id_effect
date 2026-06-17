//! Ex 035 — `plan_topological` orders capability providers by dependency.

use id_effect::{PlannerNode, plan_topological};

fn main() {
  let nodes = vec![
    PlannerNode::new("a", Vec::<&str>::new(), "A"),
    PlannerNode::new("b", ["A"], "B"),
  ];
  let plan = plan_topological(&nodes).expect("plan");
  assert!(plan.build_order.contains(&"a".to_owned()));
  println!("035_layer_graph ok");
}
