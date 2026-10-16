# Supervised daemon skeleton

**Problem.** A long-running service has workers that can crash on bad input. A crash should
restart the worker with clean state instead of taking the daemon down. On SIGTERM the daemon
should stop in order and let workers flush what they hold.

**Recipe.** Use the OTP layer, `id_effect_process`:

- a `Worker` implementing `Process`, with state that changes as it handles casts,
- a `SupervisorSpec` with `Strategy::OneForOne` and a restart intensity,
- `SupervisorHandle::stop` on shutdown. It sends each child a trappable `Shutdown`, escalates to a
  kill after the child's shutdown timeout, and runs the worker's `terminate` hook.

```rust
# use std::sync::Arc;
# use std::sync::atomic::{AtomicU64, Ordering};
# use id_effect::{Effect, Never, fail, succeed};
# use id_effect_process::{ExitReason, Next, Process, ProcessCtx};
enum Job { Work(u64), Poison }

struct Worker { total: u64, flushed: Arc<AtomicU64> }

impl Process for Worker {
  type Args = Arc<AtomicU64>;
  type Env = ();
  type Call = ();
  type Reply = u64;
  type Cast = Job;
  type Info = ();
  type Error = String;

  fn init(flushed: Arc<AtomicU64>, _ctx: ProcessCtx) -> Effect<Self, String, ()> {
    succeed(Worker { total: 0, flushed })
  }

  fn handle_call(self, _: (), _ctx: ProcessCtx) -> Effect<(u64, Next<Self>), String, ()> {
    let total = self.total;
    succeed((total, Next::Continue(self)))
  }

  fn handle_cast(self, job: Job, _ctx: ProcessCtx) -> Effect<Next<Self>, String, ()> {
    match job {
      Job::Work(n) => succeed(Next::Continue(Worker { total: self.total + n, ..self })),
      // A failed handler crashes the process; the supervisor gets an exit signal.
      Job::Poison => fail("poison message".to_string()),
    }
  }

  fn terminate(self, reason: &ExitReason, _ctx: ProcessCtx) -> Effect<(), Never, ()> {
    self.flushed.store(self.total, Ordering::SeqCst); // flush on graceful stop
    succeed(())
  }
}
```

Register the worker under a name in its start function, so callers always reach the current
incarnation after a restart:

```rust
# use std::sync::Arc;
# use std::sync::atomic::{AtomicU64, Ordering};
# use std::time::Duration;
# use id_effect::{Effect, HNil, Never, fail, succeed};
# use id_effect_process::{
#   ChildSpec, ExitReason, Next, NodeName, Pid, Process, ProcessCtx, ProcessNode, ShutdownPolicy,
#   StartFn, Strategy, SupervisorHandle, SupervisorSpec,
# };
# enum Job { Work(u64), Poison }
# struct Worker { total: u64, flushed: Arc<AtomicU64> }
# impl Process for Worker {
#   type Args = Arc<AtomicU64>; type Env = (); type Call = (); type Reply = u64;
#   type Cast = Job; type Info = (); type Error = String;
#   fn init(flushed: Arc<AtomicU64>, _ctx: ProcessCtx) -> Effect<Self, String, ()> { succeed(Worker { total: 0, flushed }) }
#   fn handle_call(self, _: (), _ctx: ProcessCtx) -> Effect<(u64, Next<Self>), String, ()> {
#     let total = self.total;
#     succeed((total, Next::Continue(self)))
#   }
#   fn handle_cast(self, job: Job, _ctx: ProcessCtx) -> Effect<Next<Self>, String, ()> {
#     match job {
#       Job::Work(n) => succeed(Next::Continue(Worker { total: self.total + n, ..self })),
#       Job::Poison => fail("poison message".to_string()),
#     }
#   }
#   fn terminate(self, _reason: &ExitReason, _ctx: ProcessCtx) -> Effect<(), Never, ()> {
#     self.flushed.store(self.total, Ordering::SeqCst);
#     succeed(())
#   }
# }
# fn worker_pid(tree: &SupervisorHandle) -> Option<Pid> {
#   tree.which_children(Duration::from_secs(5)).block_on().into_result().ok()?
#     .into_iter().find(|(id, _)| id == "worker").and_then(|(_, pid)| pid)
# }
# let flushed = Arc::new(AtomicU64::new(0));
fn worker_child(flushed: Arc<AtomicU64>) -> ChildSpec {
  let start: StartFn = Arc::new(move |node: &ProcessNode, supervisor: &Pid| {
    let addr = node.spawn_linked_to::<Worker>(supervisor, Arc::clone(&flushed), ());
    node.register("worker", &addr);
    Ok(addr.pid().clone())
  });
  ChildSpec::from_start("worker", start)
}

let node = ProcessNode::new(NodeName::new("daemon@local"));
let tree = SupervisorSpec::new(Strategy::OneForOne)
  .intensity(3, Duration::from_secs(5)) // more than 3 restarts in 5 s escalates
  .child(worker_child(Arc::clone(&flushed)))
  .start(&node);

let first = worker_pid(&tree).expect("worker started"); // via `tree.which_children(..)`
let worker = node.whereis_addr::<Worker>("worker").expect("registered");
# let _ = (first, worker);
# tree.stop::<Never, HNil>(&node, ShutdownPolicy::Timeout(Duration::from_secs(5))).block_on();
```

`Process` handlers return ordinary 0.7 effects (`Effect<_, Self::Error, ()>`), and every
node / supervisor operation (`cast`, `call`, `which_children`, `stop`, `await_exit`) is an
effect you run with `.block_on()` from `main` or compose with other effects. The example waits
for the crashed incarnation without sleeping:

```rust
# use std::sync::Arc;
# use std::sync::atomic::{AtomicU64, Ordering};
# use std::time::Duration;
# use id_effect::{Effect, HNil, Never, fail, succeed};
# use id_effect_process::{
#   ChildSpec, ExitReason, Next, NodeName, Pid, Process, ProcessCtx, ProcessNode, ShutdownPolicy,
#   StartFn, Strategy, SupervisorHandle, SupervisorSpec,
# };
# enum Job { Work(u64), Poison }
# struct Worker { total: u64, flushed: Arc<AtomicU64> }
# impl Process for Worker {
#   type Args = Arc<AtomicU64>; type Env = (); type Call = (); type Reply = u64;
#   type Cast = Job; type Info = (); type Error = String;
#   fn init(flushed: Arc<AtomicU64>, _ctx: ProcessCtx) -> Effect<Self, String, ()> { succeed(Worker { total: 0, flushed }) }
#   fn handle_call(self, _: (), _ctx: ProcessCtx) -> Effect<(u64, Next<Self>), String, ()> {
#     let total = self.total;
#     succeed((total, Next::Continue(self)))
#   }
#   fn handle_cast(self, job: Job, _ctx: ProcessCtx) -> Effect<Next<Self>, String, ()> {
#     match job {
#       Job::Work(n) => succeed(Next::Continue(Worker { total: self.total + n, ..self })),
#       Job::Poison => fail("poison message".to_string()),
#     }
#   }
#   fn terminate(self, _reason: &ExitReason, _ctx: ProcessCtx) -> Effect<(), Never, ()> {
#     self.flushed.store(self.total, Ordering::SeqCst);
#     succeed(())
#   }
# }
# fn worker_child(flushed: Arc<AtomicU64>) -> ChildSpec {
#   let start: StartFn = Arc::new(move |node: &ProcessNode, supervisor: &Pid| {
#     let addr = node.spawn_linked_to::<Worker>(supervisor, Arc::clone(&flushed), ());
#     node.register("worker", &addr);
#     Ok(addr.pid().clone())
#   });
#   ChildSpec::from_start("worker", start)
# }
# fn worker_pid(tree: &SupervisorHandle) -> Option<Pid> {
#   tree.which_children(Duration::from_secs(5)).block_on().into_result().ok()?
#     .into_iter().find(|(id, _)| id == "worker").and_then(|(_, pid)| pid)
# }
# let flushed = Arc::new(AtomicU64::new(0));
# let node = ProcessNode::new(NodeName::new("daemon@local"));
# let tree = SupervisorSpec::new(Strategy::OneForOne)
#   .intensity(3, Duration::from_secs(5))
#   .child(worker_child(Arc::clone(&flushed)))
#   .start(&node);
# let first = worker_pid(&tree).expect("worker started");
# let worker = node.whereis_addr::<Worker>("worker").expect("registered");
worker.cast(Job::Poison).block_on(); // crash: the supervisor restarts it with total = 0
node.await_exit::<Never, HNil>(&first).block_on(); // its exit signal is now with the supervisor
let second = worker_pid(&tree).expect("supervisor restarted the worker");
assert_ne!(first, second);
# tree.stop::<Never, HNil>(&node, ShutdownPolicy::Timeout(Duration::from_secs(5))).block_on();
```

## Wiring the shutdown signal

The runnable example simulates the signal with a channel so that it stays deterministic. In a
binary, wait for the OS signal instead. With Tokio's `signal` feature and an async `main`, await
the stop effect through a `Runtime` (`block_on` refuses to run inside an async context):

<!-- ignore: needs tokio's `signal` feature; tokio is not a dependency of the book_snippets harness -->
```rust,ignore
#[tokio::main]
async fn main() {
  let (node, tree) = start_tree();
  tokio::signal::ctrl_c().await.ok(); // or tokio::signal::unix::signal(SignalKind::terminate())
  let stop = tree.stop::<Never, HNil>(&node, ShutdownPolicy::Timeout(Duration::from_secs(5)));
  Runtime::global().run(stop).await;
}
```

`stop` returns after every child has exited, so when it returns the worker's `terminate` hook has
already flushed its state.

Full example: [`crates/id_effect_process/examples/154_cookbook_supervised_daemon.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_process/examples/154_cookbook_supervised_daemon.rs)

```bash
cargo run -p id_effect_process --example 154_cookbook_supervised_daemon
```

## Variations

- **A state-machine child.** `id_effect_fsm::FsmProcess` hosts a typed state machine as a
  `gen_statem`-style process, so it slots into the same tree. See
  `cargo run -p id_effect_fsm --example supervised_traffic_light`.
- **Many identical workers.** Use `DynamicSupervisor` for children started on demand.
- **Workers across machines.** Move up to `id_effect_node::DistributedSupervisor`
  ([Cluster groups and takeover](../part7/ch39-00-cluster-supervision.md)).

Which layer to choose is covered in [Supervision layering](../part3/ch09-05-supervision.md#supervision-layering).
