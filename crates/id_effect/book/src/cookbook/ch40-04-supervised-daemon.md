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
fn worker_child(flushed: Arc<AtomicU64>) -> ChildSpec {
  let start: StartFn = Arc::new(move |node: &ProcessNode, supervisor: &Pid| {
    let addr = node.spawn_linked_to::<Worker>(supervisor, Arc::clone(&flushed), ());
    node.register("worker", &addr);
    Ok(addr.pid().clone())
  });
  ChildSpec::from_start("worker", start)
}

let node = ProcessNode::threads(NodeName::new("daemon@local"));
let tree = SupervisorSpec::new(Strategy::OneForOne)
  .intensity(3, Duration::from_secs(5)) // more than 3 restarts in 5 s escalates
  .child(worker_child(Arc::clone(&flushed)))
  .start(&node);

let worker = node.whereis_addr::<Worker>("worker").expect("registered");
run_blocking(worker.cast(Job::Poison), ())?; // crash → supervisor restarts it with total = 0
```

## Wiring the shutdown signal

The runnable example simulates the signal with a channel so that it stays deterministic. In a
binary, feed the same channel from the OS. With Tokio's `signal` feature:

```rust
let (signal_tx, signal_rx) = std::sync::mpsc::channel();
tokio::spawn(async move {
  tokio::signal::ctrl_c().await.ok(); // or tokio::signal::unix::signal(SignalKind::terminate())
  let _ = signal_tx.send("SIGTERM");
});

let signal = signal_rx.recv().expect("signal");
tree.stop(&node, ShutdownPolicy::Timeout(Duration::from_secs(5))).await;
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
