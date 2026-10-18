//! Example 135 — cluster-wide process names: register on one node, resolve and call by name
//! from another, and watch the name go away when the process exits.

use std::time::Duration;

use id_effect::{Effect, Never, succeed, sync};
use id_effect_node::{BehaviorRegistry, Mesh, MeshFabric, NodeId};
use id_effect_process::{ExitReason, Next, NodeName, Process, ProcessCtx, ProcessNode};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
enum Call {
  Get,
}

#[derive(Serialize, Deserialize)]
enum Cast {
  Add(u32),
}

struct Tally {
  n: u32,
}

impl Process for Tally {
  type Args = ();
  type Env = ();
  type Call = Call;
  type Reply = u32;
  type Cast = Cast;
  type Info = ();
  type Error = Never;

  fn init(_: (), _: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Tally { n: 0 })
  }

  fn handle_call(self, msg: Call, _: ProcessCtx) -> Effect<(u32, Next<Self>), Never, ()> {
    sync(move || match msg {
      Call::Get => (self.n, Next::Continue(self)),
    })
  }

  fn handle_cast(mut self, msg: Cast, _: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    sync(move || match msg {
      Cast::Add(n) => {
        self.n += n;
        Next::Continue(self)
      }
    })
  }
}

fn node(fabric: &MeshFabric, name: &str) -> std::sync::Arc<Mesh> {
  let mut registry = BehaviorRegistry::new();
  registry.register::<Tally>("tally");
  Mesh::new(
    NodeId::fresh(&NodeName::new(name)),
    ProcessNode::new(NodeName::new(name)),
    registry,
  )
  .with_fabric(fabric.clone())
}

fn main() {
  let fabric = MeshFabric::new();
  let a = node(&fabric, "a@ex");
  let b = node(&fabric, "b@ex");

  // The hosting node claims the name; every node's replica learns it.
  let pid = b
    .spawn_local("tally", &postcard::to_allocvec(&()).unwrap(), None)
    .expect("spawn");
  b.global().register("tally", &pid).expect("register");
  println!("a sees tally at {:?}", a.global().whereis("tally"));
  // A second claim of the same name is refused, from any node.
  let other = a
    .spawn_local("tally", &postcard::to_allocvec(&()).unwrap(), None)
    .expect("spawn");
  println!("a claims tally: {:?}", a.global().register("tally", &other));

  // Reach the process by name from the other node.
  a.global()
    .cast("tally", postcard::to_allocvec(&Cast::Add(42)).unwrap())
    .unwrap();
  let bytes = a
    .global()
    .call(
      "tally",
      postcard::to_allocvec(&Call::Get).unwrap(),
      Duration::from_secs(2),
    )
    .block_on()
    .into_result()
    .expect("call by name");
  let n: u32 = postcard::from_bytes(&bytes).unwrap();
  println!("tally (by name, from a) = {n}");

  // The name is released when its process exits.
  let exited: Effect<ExitReason, Never> = b.process().await_exit(&pid);
  b.process().exit(&pid, ExitReason::Killed);
  let _ = exited.block_on();
  println!(
    "after exit, a sees tally at {:?}",
    a.global().whereis("tally")
  );
}
