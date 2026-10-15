//! `FsmProcess`: the gen_statem-style behaviour, driven through real
//! `id_effect_process` nodes. No sleeps — exits are observed through monitors.

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::next::{HCons, HNil, fail, need, sleep};
use id_effect::{Cause, Exit};
use id_effect_fsm::{
  Fsm, FsmError, FsmHandle, FsmProcess, FsmSpec, Interpreter, StateMachine, TransitionTable,
  Unhandled,
};
use id_effect_process::{
  CallError, ChildSpec, ExitReason, NodeName, Pid, ProcessNode, StartFn, Strategy, SupervisorSpec,
};

const T: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Ev {
  Coin,
  Push,
  Break,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Fsm)]
#[fsm(event = Ev, initial = Locked)]
enum Turnstile {
  #[fsm(on(Coin => Unlocked, Break => Broken))]
  Locked,
  #[fsm(on(Push => Locked, Break => Broken))]
  Unlocked,
  Broken,
}

type Handle<R> = FsmHandle<Turnstile, Ev, String, R>;

fn node(name: &str) -> ProcessNode {
  ProcessNode::new(NodeName::new(name))
}

/// Run a `next` effect at the edge (the node's processes are fibers on the default runtime).
fn run<A: Send + 'static, E: Send + 'static>(
  eff: id_effect::next::Effect<A, E, HNil>,
) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died"),
  }
}

fn start(node: &ProcessNode, spec: FsmSpec<Turnstile, Ev, String, ()>) -> Handle<()> {
  FsmProcess::start(node, spec, ())
}

fn watch_exit(node: &ProcessNode, pid: &Pid) -> Receiver<ExitReason> {
  let (tx, rx) = mpsc::channel();
  node.monitor_with_sink(
    pid,
    Box::new(move |_m, _p, reason| {
      let _ = tx.send(reason);
    }),
  );
  rx
}

fn state<R: Send + Sync + 'static>(h: &Handle<R>) -> Turnstile {
  run(h.state(T)).expect("state call")
}

#[test]
fn fire_transitions_and_state_is_queryable() {
  let node = node("fsm-fire@test");
  let h = start(&node, FsmSpec::from_fsm());
  assert_eq!(state(&h), Turnstile::Locked);
  assert_eq!(run(h.fire(Ev::Coin, T)), Ok(Ok(Turnstile::Unlocked)));
  assert_eq!(run(h.fire(Ev::Push, T)), Ok(Ok(Turnstile::Locked)));
}

#[test]
fn fire_without_an_edge_is_rejected_and_keeps_the_state() {
  let node = node("fsm-reject@test");
  let h = start(&node, FsmSpec::from_fsm());
  let out = run(h.fire(Ev::Push, T)).expect("call");
  assert_eq!(
    out,
    Err(FsmError::NoTransition {
      state: Turnstile::Locked,
      event: Ev::Push
    })
  );
  assert_eq!(state(&h), Turnstile::Locked);
}

#[test]
fn cast_and_send_events_apply_in_mailbox_order() {
  let node = node("fsm-cast@test");
  let h = start(&node, FsmSpec::from_fsm());
  run(h.cast(Ev::Coin)).unwrap();
  run(h.addr().send(Ev::Push)).unwrap();
  assert!(h.addr().send_now(Ev::Coin));
  assert_eq!(state(&h), Turnstile::Unlocked);
}

#[test]
fn unhandled_cast_is_ignored_by_default() {
  let node = node("fsm-ignore@test");
  let h = start(&node, FsmSpec::from_fsm());
  run(h.cast(Ev::Push)).unwrap();
  assert_eq!(state(&h), Turnstile::Locked);
  assert!(h.addr().is_alive());
}

#[test]
fn unhandled_cast_stops_the_process_when_configured() {
  let node = node("fsm-stop@test");
  let h = start(&node, FsmSpec::from_fsm().on_unhandled(Unhandled::Stop));
  let exits = watch_exit(&node, h.pid());
  run(h.cast(Ev::Push)).unwrap();
  match exits.recv_timeout(T).expect("exit") {
    ExitReason::Error(msg) => assert!(msg.contains("NoTransition"), "{msg}"),
    other => panic!("expected Error, got {other:?}"),
  }
}

#[test]
fn rejected_calls_never_stop_the_process_even_with_stop_policy() {
  let node = node("fsm-call-stop@test");
  let h = start(&node, FsmSpec::from_fsm().on_unhandled(Unhandled::Stop));
  assert!(matches!(run(h.fire(Ev::Push, T)), Ok(Err(_))));
  assert_eq!(state(&h), Turnstile::Locked);
}

#[test]
fn transition_actions_run_in_the_process_environment() {
  let node = node("fsm-env@test");
  type Audit = HCons<Mutex<Vec<Turnstile>>, HNil>;
  let audit: Arc<Mutex<Vec<Turnstile>>> = Arc::default();
  let actions = Interpreter::<Turnstile, Ev, (), String, Audit>::new().on_transition(
    Turnstile::Locked,
    Ev::Coin,
    || need::<Mutex<Vec<Turnstile>>>().map(|log| log.lock().unwrap().push(Turnstile::Locked)),
  );
  let h = FsmProcess::start(
    &node,
    FsmSpec::from_fsm().with_interpreter(actions),
    HCons(Arc::clone(&audit), HNil),
  );
  assert_eq!(run(h.fire(Ev::Coin, T)), Ok(Ok(Turnstile::Unlocked)));
  assert_eq!(*audit.lock().unwrap(), vec![Turnstile::Locked]);
  assert_eq!(run(h.fire(Ev::Push, T)), Ok(Ok(Turnstile::Locked)));
  assert_eq!(audit.lock().unwrap().len(), 1, "no action on Push");
}

#[test]
fn async_actions_suspend_the_process_fiber_and_then_commit() {
  let node = node("fsm-async@test");
  let actions = Interpreter::<Turnstile, Ev, (), String, ()>::new().on_transition(
    Turnstile::Locked,
    Ev::Coin,
    || sleep(Duration::from_millis(5)),
  );
  let h = start(&node, FsmSpec::from_fsm().with_interpreter(actions));
  run(h.cast(Ev::Coin)).unwrap();
  // The cast's action suspends the process; the call is handled after it, in mailbox order.
  assert_eq!(state(&h), Turnstile::Unlocked);
}

#[test]
fn failing_action_crashes_the_process_with_the_action_error() {
  let node = node("fsm-crash@test");
  let actions = Interpreter::new().on_transition(Turnstile::Locked, Ev::Break, || {
    fail("vandalism".to_string())
  });
  let h = start(&node, FsmSpec::from_fsm().with_interpreter(actions));
  let exits = watch_exit(&node, h.pid());
  let out = run(h.fire(Ev::Break, T));
  assert!(
    matches!(out, Err(CallError::Down(ExitReason::Error(_)))),
    "{out:?}"
  );
  assert_eq!(
    exits.recv_timeout(T).expect("exit"),
    ExitReason::error("vandalism")
  );
}

#[test]
fn hand_built_machines_work_without_the_derive() {
  let node = node("fsm-manual@test");
  let table = TransitionTable::new().on(0u8, 'x', 1u8).on(1, 'x', 0);
  let h: FsmHandle<u8, char, String, ()> =
    FsmProcess::start(&node, FsmSpec::new(StateMachine::new(0, table)), ());
  assert_eq!(run(h.fire('x', T)), Ok(Ok(1)));
  assert_eq!(run(h.state(T)), Ok(1));
}

#[test]
fn supervisor_restarts_a_crashed_machine_from_its_initial_state() {
  let node = node("fsm-sup@test");
  let actions = Interpreter::new().on_transition(Turnstile::Unlocked, Ev::Break, || {
    fail("motor jam".to_string())
  });
  let spec: FsmSpec<Turnstile, Ev, String, ()> = FsmSpec::from_fsm().with_interpreter(actions);
  let current: Arc<Mutex<Option<Handle<()>>>> = Arc::default();
  let publish = Arc::clone(&current);
  let start: StartFn = Arc::new(move |node, sup| {
    let addr = node.spawn_linked_to::<FsmProcess<Turnstile, Ev, String, ()>>(sup, spec.clone(), ());
    let pid = addr.pid().clone();
    *publish.lock().unwrap() = Some(FsmHandle::from_addr(addr));
    Ok(pid)
  });
  let tree = SupervisorSpec::new(Strategy::OneForOne)
    .child(ChildSpec::from_start("turnstile", start))
    .start(&node);
  run(tree.which_children(T)).unwrap();
  let first = current.lock().unwrap().clone().unwrap();
  assert_eq!(run(first.fire(Ev::Coin, T)), Ok(Ok(Turnstile::Unlocked)));
  let crashed = run(first.fire(Ev::Break, T));
  assert!(matches!(crashed, Err(CallError::Down(_))), "{crashed:?}");

  // The monitor behind `call` fires after the link exit reached the
  // supervisor, so this call is ordered after the restart.
  let kids = run(tree.which_children(T)).unwrap();
  let second = current.lock().unwrap().clone().unwrap();
  assert_eq!(kids[0].1.as_ref(), Some(second.pid()));
  assert_ne!(first.pid(), second.pid());
  assert_eq!(state(&second), Turnstile::Locked);
}

#[test]
fn start_linked_ties_the_machine_to_its_parent() {
  use id_effect::next::{Effect, succeed};
  use id_effect_process::{Next, Process, ProcessCtx};

  /// Parent that starts a linked machine in `init` and returns its pid.
  struct Parent {
    child: Pid,
  }
  impl Process for Parent {
    type Args = ();
    type Env = ();
    type Call = ();
    type Reply = Pid;
    type Cast = ();
    type Info = ();
    type Error = String;
    fn init(_: (), ctx: ProcessCtx) -> Effect<Self, String, ()> {
      let h = FsmProcess::<Turnstile, Ev, String, ()>::start_linked(&ctx, FsmSpec::from_fsm(), ());
      succeed(Parent {
        child: h.pid().clone(),
      })
    }
    fn handle_call(self, _: (), _ctx: ProcessCtx) -> Effect<(Pid, Next<Self>), String, ()> {
      let child = self.child.clone();
      succeed((child, Next::Continue(self)))
    }
  }

  let node = node("fsm-linked@test");
  let parent = node.spawn::<Parent>((), ());
  let child = run(parent.call((), T)).unwrap();
  let child_exit = watch_exit(&node, &child);
  node.exit(parent.pid(), ExitReason::Killed);
  assert_eq!(
    child_exit.recv_timeout(T).expect("exit"),
    ExitReason::Killed
  );
}
