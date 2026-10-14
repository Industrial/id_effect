//! `#[derive(Fsm)]` used from a downstream crate, through the `id_effect_fsm` re-export.

use id_effect::{Effect, run_blocking};
use id_effect_fsm::{Fsm, FsmError, Interpreter, RunError, to_mermaid};

mod events {
  #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
  pub enum Order {
    Pay,
    Ship,
    Cancel,
    Deliver,
  }
}

use events::Order;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Fsm)]
#[fsm(event = events::Order, initial = Placed)]
enum OrderState {
  #[fsm(on(Pay => Paid), on(Cancel => Cancelled))]
  Placed,
  #[fsm(on(Ship => Shipped, Cancel => Refunded))]
  Paid,
  // Fully qualified event paths are used as written.
  #[fsm(on(events::Order::Deliver => Delivered))]
  Shipped,
  Delivered,
  Cancelled,
  Refunded,
}

/// `crate = ...` lets a re-exporting crate point the generated code at its own path.
mod reexport {
  pub use id_effect_fsm as fsm;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Toggle {
  Flip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, reexport::fsm::Fsm)]
#[fsm(event = Toggle, crate = reexport::fsm)]
enum Switch {
  #[fsm(on(Flip => On))]
  Off,
  #[fsm(on(Flip => Off))]
  On,
}

#[test]
fn derived_table_contains_exactly_the_declared_edges() {
  let table = OrderState::transition_table();
  assert_eq!(table.len(), 5);
  assert_eq!(
    table.next(OrderState::Placed, Order::Pay),
    Some(OrderState::Paid)
  );
  assert_eq!(
    table.next(OrderState::Paid, Order::Cancel),
    Some(OrderState::Refunded)
  );
  assert_eq!(
    table.next(OrderState::Shipped, Order::Deliver),
    Some(OrderState::Delivered)
  );
  assert_eq!(table.next(OrderState::Delivered, Order::Cancel), None);
  assert_eq!(OrderState::initial(), OrderState::Placed);
}

#[test]
fn derived_machine_walks_the_happy_path() {
  let mut m = OrderState::state_machine();
  for event in [Order::Pay, Order::Ship, Order::Deliver] {
    m.step(event).expect("declared edge");
  }
  assert_eq!(m.state(), OrderState::Delivered);
  assert_eq!(
    m.step(Order::Cancel),
    Err(FsmError::NoTransition {
      state: OrderState::Delivered,
      event: Order::Cancel
    })
  );
}

#[test]
fn crate_override_resolves_through_a_reexport() {
  let mut m = Switch::state_machine();
  assert_eq!(m.state(), Switch::Off);
  assert_eq!(m.step(Toggle::Flip), Ok(Switch::On));
  assert_eq!(m.step(Toggle::Flip), Ok(Switch::Off));
}

#[test]
fn derived_machines_drive_effectful_interpreters() {
  let refunds = Interpreter::<OrderState, Order, (), String, u32>::new().on_transition(
    OrderState::Paid,
    Order::Cancel,
    || {
      Effect::new(|refunded: &mut u32| {
        *refunded += 1;
        Ok(())
      })
    },
  );
  let program = refunds
    .run(OrderState::state_machine(), [Order::Pay, Order::Cancel])
    .flat_map(|m| Effect::new(move |refunded: &mut u32| Ok((m.state(), *refunded))));
  assert_eq!(run_blocking(program, 0), Ok((OrderState::Refunded, 1)));

  let rejected = run_blocking(refunds.run(OrderState::state_machine(), [Order::Ship]), 0);
  assert!(matches!(rejected, Err(RunError::Transition(_))));
}

#[test]
fn derived_machines_render_as_mermaid() {
  let chart = to_mermaid(
    &OrderState::state_machine(),
    |s| format!("{s:?}"),
    |e| format!("{e:?}"),
  );
  assert!(chart.contains("[*] --> Placed"));
  assert!(chart.contains("Paid --> Refunded: Cancel"));
}
