//! Property tests for [`topological_sort`].

use id_effect_graph::{DependencyNode, topological_sort};
use proptest::prelude::*;

proptest! {
  #[test]
  fn linear_chain_orders_left_to_right(n in 2usize..8) {
    let mut nodes = vec![DependencyNode::new("n0", Vec::<&str>::new(), "c0")];
    for i in 1..n {
      nodes.push(DependencyNode::new(
        format!("n{i}"),
        [format!("c{}", i - 1)],
        format!("c{i}"),
      ));
    }
    let order = topological_sort(&nodes).expect("linear chain acyclic");
    for i in 1..n {
      let prev = format!("n{}", i - 1);
      let cur = format!("n{i}");
      let pos = |x: &str| order.iter().position(|s| s == x).unwrap();
      prop_assert!(pos(&prev) < pos(&cur));
    }
  }

  #[test]
  fn two_node_cycle_fails(_seed in 0u8..1) {
    let nodes = vec![
      DependencyNode::new("one", ["B"], "A"),
      DependencyNode::new("two", ["A"], "B"),
    ];
    prop_assert!(topological_sort(&nodes).is_err());
  }
}
