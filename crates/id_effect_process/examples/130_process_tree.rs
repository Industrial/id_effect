//! Example 130 — local process supervision tree.

use std::time::Duration;

use id_effect::Never;
use id_effect::next::{Effect, HNil, succeed};
use id_effect_process::{
  ChildSpec, Next, NodeName, Process, ProcessCtx, ProcessNode, Strategy, SupervisorSpec,
};

struct Ping;

impl Process for Ping {
  type Args = ();
  type Env = ();
  type Call = ();
  type Reply = &'static str;
  type Cast = ();
  type Info = ();
  type Error = Never;

  fn init(_: (), _: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Ping)
  }

  fn handle_call(self, _: (), _: ProcessCtx) -> Effect<(&'static str, Next<Self>), Never, ()> {
    succeed(("pong", Next::Continue(self)))
  }
}

fn main() {
  // Processes are fibers on the default `next` runtime.
  let node = ProcessNode::new(NodeName::new("ex130@local"));
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .child(ChildSpec::worker::<Ping, _>("ping", || ((), ())))
    .start(&node);
  // A supervisor call is answered only after `init` started every child.
  let children = handle
    .which_children::<HNil>(Duration::from_secs(5))
    .block_on()
    .into_result()
    .expect("supervisor answers");
  println!(
    "supervisor pid {} children={children:?} live processes={}",
    handle.pid(),
    node.table().len()
  );
}
