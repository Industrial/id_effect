//! The stage model, run through the kit's effect runtime (r13-t0-skeletons).

use pipeline::{Stage, stages};

#[test]
fn pipeline_stages_run_through_the_kit() {
  let all = stages().block_on().into_result().unwrap();
  assert_eq!(all, Stage::ALL);
  assert_eq!(Stage::Publish.next(), None);
}
