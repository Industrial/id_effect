//! Ex 075 — `backpressure_decision` maps policy + fill to an action.
use id_effect::{BackpressureDecision, BackpressurePolicy, backpressure_decision};

fn main() {
  assert_eq!(
    backpressure_decision(BackpressurePolicy::BoundedBlock, 4, 4),
    BackpressureDecision::Block
  );
  assert_eq!(
    backpressure_decision(BackpressurePolicy::Fail, 4, 4),
    BackpressureDecision::Fail
  );
  println!("075_backpressure_decision ok");
}
