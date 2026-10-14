//! A door state machine declared with `#[derive(Fsm)]`, with Effect-valued transition
//! actions, run both as a plain Effect program and as a supervised `FsmProcess`.
//!
//! ```bash
//! cargo run -p id_effect_fsm --example door_fsm
//! ```

use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{Effect, run_blocking};
use id_effect_fsm::{Fsm, FsmProcess, FsmSpec, Interpreter, to_mermaid};
use id_effect_process::{NodeName, ProcessNode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DoorEvent {
  Open,
  Close,
  Lock,
  Unlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Fsm)]
#[fsm(event = DoorEvent, initial = Closed)]
enum Door {
  #[fsm(on(Open => Opened, Lock => Locked))]
  Closed,
  #[fsm(on(Close => Closed))]
  Opened,
  #[fsm(on(Unlock => Closed))]
  Locked,
}

/// Audit log shared by every transition action.
type Log = Arc<Mutex<Vec<String>>>;

fn audited(
  log: &Log,
  line: &'static str,
) -> impl Fn() -> Effect<(), String, ()> + Send + Sync + 'static {
  let log = Arc::clone(log);
  move || {
    let log = Arc::clone(&log);
    Effect::new(move |_| {
      log.lock().unwrap().push(line.to_string());
      Ok(())
    })
  }
}

fn interpreter(log: &Log) -> Interpreter<Door, DoorEvent, (), String, ()> {
  Interpreter::new()
    .on_transition(Door::Closed, DoorEvent::Lock, audited(log, "bolt engaged"))
    .on_transition(
      Door::Locked,
      DoorEvent::Unlock,
      audited(log, "bolt released"),
    )
}

fn main() {
  println!(
    "{}",
    to_mermaid(
      &Door::state_machine(),
      |s| format!("{s:?}"),
      |e| format!("{e:?}")
    )
  );

  // 1. As an Effect program: transitions (and their actions) run when interpreted.
  let log: Log = Arc::default();
  let program = interpreter(&log).run(
    Door::state_machine(),
    [DoorEvent::Lock, DoorEvent::Unlock, DoorEvent::Open],
  );
  let door = run_blocking(program, ()).expect("valid event sequence");
  println!(
    "effect program ended in {:?}; audit: {:?}",
    door.state(),
    log.lock().unwrap()
  );

  // 2. As a gen_statem-style process: events are mailbox messages.
  let node = ProcessNode::threads(NodeName::new("door@local"));
  let spec = FsmSpec::from_fsm().with_interpreter(interpreter(&log));
  let door = FsmProcess::start(&node, spec, ());
  let timeout = Duration::from_secs(5);
  run_blocking(door.cast(DoorEvent::Lock), ()).expect("mailbox open");
  let rejected = run_blocking(door.fire(DoorEvent::Open, timeout), ()).expect("call");
  println!("open while locked: {rejected:?}");
  let state = run_blocking(door.state(timeout), ()).expect("call");
  println!("process state: {state:?}; audit: {:?}", log.lock().unwrap());
}
