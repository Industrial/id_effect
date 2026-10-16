# Processes and supervision trees

A `Process` is a GenServer-shaped behavior: `init`, `handle_call`, `handle_cast`, `handle_info`, `terminate` — each returns an `Effect` over the process environment `Env` (provided once at spawn). Every process is a fiber on an `id_effect::Runtime`; `ProcessNode::new` uses `Runtime::global()`.

```rust
use id_effect::{Effect, Never, succeed};
use id_effect_process::{Next, NodeName, Process, ProcessCtx, ProcessNode};

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

let node = ProcessNode::new(NodeName::new("app@local"));
```

`Addr::{call, cast, send}` return effects generic over the caller's environment; run them at the edge with `block_on` / `Runtime::run`, or compose them into a larger program.

Supervision:

```rust
use std::time::Duration;
use id_effect::HNil;
use id_effect_process::{ChildSpec, Strategy, SupervisorSpec};

# use id_effect::{Cause, Effect, Never, succeed};
# use id_effect_process::{CallError, Next, NodeName, Process, ProcessCtx, ProcessNode};
# struct Ping;
# impl Process for Ping {
#   type Args = ();
#   type Env = ();
#   type Call = ();
#   type Reply = &'static str;
#   type Cast = ();
#   type Info = ();
#   type Error = Never;
#   fn init(_: (), _: ProcessCtx) -> Effect<Self, Never, ()> {
#     succeed(Ping)
#   }
#   fn handle_call(self, _: (), _: ProcessCtx) -> Effect<(&'static str, Next<Self>), Never, ()> {
#     succeed(("pong", Next::Continue(self)))
#   }
# }
# fn main() -> Result<(), Cause<CallError>> {
# let node = ProcessNode::new(NodeName::new("app@local"));
let handle = SupervisorSpec::new(Strategy::OneForOne)
  .intensity(3, Duration::from_secs(5))
  .child(ChildSpec::worker::<Ping, _>("ping", || ((), ())))
  .start(&node);
let children = handle
  .which_children::<HNil>(Duration::from_secs(5))
  .block_on()
  .into_result()?;
# assert_eq!(children.len(), 1);
# Ok(())
# }
```

Restarts are immediate (OTP); the budget is a sliding window of at most `max_restarts` within `within`, the shared [`RestartIntensity`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/coordination/supervisor.rs) primitive of `id_effect`, so local and clustered supervisors share one intensity definition.

Time comes from the runtime's executor clock. For deterministic restart-storm and timeout tests, run the node on a `TestExecutor` and drive virtual time:

```rust
use std::sync::Arc;
use id_effect::TestExecutor;
# use std::time::Duration;
# use id_effect_process::{NodeName, ProcessNode};

let exec = TestExecutor::new();
let node = ProcessNode::on_executor(NodeName::new("t@test"), Arc::new(exec.clone()));
exec.run_until_stalled();
exec.advance(Duration::from_secs(5)); // fires shutdown deadlines, slides intensity windows
```

**Runnable examples:** `cargo run -p id_effect_process --example 130_process_tree` and `154_cookbook_supervised_daemon`.
