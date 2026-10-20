//! `Statem`: gen_statem parity (fault.fsm #3 state data, #4 state/event timeouts, #5 postpone,
//! state enter), driven through real `id_effect_process` nodes on a `TestExecutor`: time is
//! virtual and nothing sleeps. The machine logs into a shared vector, so reading the log sends
//! no message (a message would itself be an event).

use std::cell::RefCell;
use std::pin::pin;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::{Effect, Exit, HNil, TestExecutor, succeed};
use id_effect_fsm::{
  Action, Entered, Statem, StatemEvent, StatemHandle, StatemInit, StatemProcess, Transition,
};
use id_effect_process::{
  CallError, ChildSpec, ExitReason, NodeName, Pid, ProcessCtx, ProcessNode, StartFn, Strategy,
  SupervisorSpec,
};

const T: Duration = Duration::from_secs(10);

// ── Fixture: a node on a test executor ──────────────────────────────

thread_local! {
  static EXEC: RefCell<Option<TestExecutor>> = const { RefCell::new(None) };
}

fn exec() -> TestExecutor {
  EXEC.with(|e| e.borrow().clone().expect("no node on this test thread"))
}

fn node(name: &str) -> ProcessNode {
  let exec = TestExecutor::new();
  EXEC.with(|e| *e.borrow_mut() = Some(exec.clone()));
  ProcessNode::on_executor(NodeName::new(name), Arc::new(exec))
}

fn settle() {
  exec().run_until_stalled();
}

fn advance(secs: u64) {
  exec().advance(Duration::from_secs(secs));
  settle();
}

/// Run `eff` on the test executor until it stalls; it must have finished by then.
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E, HNil>) -> Result<A, E> {
  let exec = exec();
  let handle = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  let mut join = pin!(handle.join_future());
  match join.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(Exit::Success(a)) => Ok(a),
    Poll::Ready(Exit::Failure(id_effect::Cause::Fail(e))) => Err(e),
    Poll::Ready(Exit::Failure(_)) => panic!("effect died"),
    Poll::Pending => panic!("effect still pending after the executor stalled"),
  }
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

// ── The machine under test ──────────────────────────────────────────

type Log = Arc<Mutex<Vec<String>>>;

/// States carry data: no `Copy`, not even `Eq` on the payload type is needed beyond `PartialEq`.
#[derive(Clone, Debug, PartialEq)]
enum St {
  Idle,
  Busy { job: String },
}

#[derive(Clone, Debug, PartialEq)]
enum Ev {
  Go(St),
  Stay,
  /// Postponed while `Busy`, logged while `Idle`.
  Work(u32),
  ArmState(u64),
  ArmEvent(u64),
  ArmGeneric(u64),
  Fired(&'static str),
  /// Call: replies `"<state>/<handled>"`; postponed while `Busy` when `true`.
  Ask(bool),
  /// Inserts `Work(99)` and moves to `Idle`.
  Chain,
  Crash,
}

/// The data: deliberately not `Clone`.
struct Lab {
  log: Log,
  handled: u32,
}

impl Lab {
  fn push(&self, entry: String) {
    self.log.lock().expect("log").push(entry);
  }
}

struct Machine;

fn secs(n: u64) -> Duration {
  Duration::from_secs(n)
}

impl Statem for Machine {
  type Args = Log;
  type Env = ();
  type State = St;
  type Data = Lab;
  type Event = Ev;
  type Reply = String;
  type Error = String;

  fn init(log: Log, _ctx: ProcessCtx) -> Effect<StatemInit<Self>, String, ()> {
    succeed((St::Idle, Lab { log, handled: 0 }, Vec::new()))
  }

  fn handle_event(
    event: &StatemEvent<Self>,
    state: &St,
    mut data: Lab,
    _ctx: ProcessCtx,
  ) -> Effect<Transition<Self>, String, ()> {
    data.handled += 1;
    let keep = |data, actions| succeed(Transition::Keep(data, actions));
    match (event, state) {
      (StatemEvent::Call(Ev::Ask(true), _), St::Busy { .. }) => keep(data, vec![Action::Postpone]),
      (StatemEvent::Call(Ev::Ask(_), to), _) => {
        let answer = format!("{state:?}/{}", data.handled);
        keep(data, vec![Action::Reply(to.clone(), answer)])
      }
      (StatemEvent::StateTimeout(Ev::Fired(what)), _)
      | (StatemEvent::EventTimeout(Ev::Fired(what)), _)
      | (StatemEvent::GenericTimeout(_, Ev::Fired(what)), _) => {
        data.push(format!("timeout {what} in {state:?}"));
        keep(data, Vec::new())
      }
      (e, _) => match e.content() {
        Ev::Go(next) => succeed(Transition::Next(next.clone(), data, Vec::new())),
        Ev::Stay => keep(data, Vec::new()),
        Ev::Work(_) if matches!(state, St::Busy { .. }) => keep(data, vec![Action::Postpone]),
        Ev::Work(n) => {
          let origin = if matches!(e, StatemEvent::Internal(_)) {
            "internal"
          } else {
            "mailbox"
          };
          data.push(format!("work {n} ({origin})"));
          keep(data, Vec::new())
        }
        Ev::ArmState(s) => keep(
          data,
          vec![Action::StateTimeout(secs(*s), Ev::Fired("state"))],
        ),
        Ev::ArmEvent(s) => keep(
          data,
          vec![Action::EventTimeout(secs(*s), Ev::Fired("event"))],
        ),
        Ev::ArmGeneric(s) => keep(
          data,
          vec![Action::GenericTimeout("g", secs(*s), Ev::Fired("generic"))],
        ),
        Ev::Chain => succeed(Transition::Next(
          St::Idle,
          data,
          vec![Action::NextEvent(Ev::Work(99))],
        )),
        Ev::Crash => succeed(Transition::Stop(ExitReason::error("crash requested"))),
        Ev::Fired(_) | Ev::Ask(_) => keep(data, Vec::new()),
      },
    }
  }

  fn on_enter(
    from: Option<&St>,
    state: &St,
    data: Lab,
    _ctx: ProcessCtx,
  ) -> Effect<Entered<Self>, String, ()> {
    data.push(format!("enter {from:?} -> {state:?}"));
    succeed((data, Vec::new()))
  }
}

type Handle = StatemHandle<Machine>;

fn start(name: &str) -> (ProcessNode, Handle, Log) {
  let node = node(name);
  let log = Log::default();
  let h = StatemProcess::<Machine>::start(&node, Arc::clone(&log), ());
  settle();
  (node, h, log)
}

fn cast(h: &Handle, ev: Ev) {
  run(h.cast(ev)).expect("cast");
  settle();
}

fn logged(log: &Log) -> Vec<String> {
  log.lock().expect("log").clone()
}

/// The log without the state-enter entries.
fn events(log: &Log) -> Vec<String> {
  logged(log)
    .into_iter()
    .filter(|e| !e.starts_with("enter"))
    .collect()
}

fn busy(job: &str) -> St {
  St::Busy { job: job.into() }
}

// ── Tests ───────────────────────────────────────────────────────────

#[test]
fn statem_state_carries_data() {
  let (_node, h, _log) = start("statem-data@test");
  assert_eq!(run(h.state(T)), Ok(St::Idle));
  cast(&h, Ev::Go(busy("paint")));
  assert_eq!(run(h.state(T)), Ok(busy("paint")));
  cast(&h, Ev::Go(busy("sand")));
  assert_eq!(
    run(h.state(T)),
    Ok(busy("sand")),
    "same variant, new payload"
  );
  // The separate `Data` (not `Clone`) travelled through every event: 2 casts + this call.
  assert_eq!(
    run(h.call(Ev::Ask(false), T)),
    Ok(format!("{:?}/3", busy("sand")))
  );
}

#[test]
fn postponed_event_redelivered_after_state_change() {
  let (_node, h, log) = start("statem-postpone@test");
  cast(&h, Ev::Go(busy("x")));
  cast(&h, Ev::Work(1));
  run(h.send(Ev::Work(2))).expect("send");
  cast(&h, Ev::Work(3));
  cast(&h, Ev::Stay);
  cast(&h, Ev::Go(busy("x")));
  assert_eq!(
    events(&log),
    Vec::<String>::new(),
    "Keep and a Next to an equal state are not state changes"
  );
  cast(&h, Ev::Go(busy("y")));
  assert_eq!(
    events(&log),
    Vec::<String>::new(),
    "re-delivered in Busy and postponed again"
  );
  cast(&h, Ev::Go(St::Idle));
  assert_eq!(
    events(&log),
    vec!["work 1 (mailbox)", "work 2 (mailbox)", "work 3 (mailbox)"],
    "arrival order after the change"
  );
  let entered = logged(&log);
  let idle = entered
    .iter()
    .position(|e| e.ends_with("-> Idle") && e.contains("Some"))
    .expect("entered Idle");
  assert!(
    idle
      < entered
        .iter()
        .position(|e| e == "work 1 (mailbox)")
        .unwrap(),
    "the enter callback runs before re-delivered events"
  );
}

#[test]
fn postponed_call_is_answered_after_state_change() {
  let (_node, h, _log) = start("statem-postpone-call@test");
  cast(&h, Ev::Go(busy("x")));
  let waiting = h.call(Ev::Ask(true), T).fork_root(Arc::new(exec()));
  settle();
  cast(&h, Ev::Go(St::Idle));
  let mut join = pin!(waiting.join_future());
  match join.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(Exit::Success(answer)) => assert_eq!(answer, "Idle/4"),
    Poll::Ready(_) => panic!("call failed"),
    Poll::Pending => panic!("postponed call not answered after the state change"),
  }
}

#[test]
fn state_timeout_cancelled_on_transition() {
  let (_node, h, log) = start("statem-state-timeout@test");
  cast(&h, Ev::ArmState(5));
  advance(4);
  cast(&h, Ev::Stay);
  advance(1);
  assert_eq!(
    events(&log),
    vec!["timeout state in Idle"],
    "survives events that keep the state"
  );

  cast(&h, Ev::ArmState(5));
  advance(4);
  cast(&h, Ev::Go(busy("x")));
  advance(60);
  assert_eq!(
    events(&log),
    vec!["timeout state in Idle"],
    "a state change cancelled it"
  );

  // Re-arming replaces: only the second one fires, once.
  cast(&h, Ev::ArmState(5));
  advance(3);
  cast(&h, Ev::ArmState(5));
  advance(3);
  assert_eq!(events(&log).len(), 1);
  advance(2);
  assert_eq!(
    events(&log).last().unwrap(),
    &format!("timeout state in {:?}", busy("x"))
  );
  advance(60);
  assert_eq!(events(&log).len(), 2);
}

#[test]
fn event_timeout_cancelled_by_any_event() {
  let (_node, h, log) = start("statem-event-timeout@test");
  cast(&h, Ev::ArmEvent(5));
  advance(5);
  assert_eq!(events(&log), vec!["timeout event in Idle"]);

  cast(&h, Ev::ArmEvent(5));
  advance(4);
  run(h.send(Ev::Stay)).expect("send");
  settle();
  advance(60);
  assert_eq!(events(&log).len(), 1, "an info event cancelled it");

  // A generic timeout firing is an event too; the generic one is not cancelled by events.
  cast(&h, Ev::ArmGeneric(3));
  cast(&h, Ev::ArmEvent(5));
  cast(&h, Ev::Go(busy("x")));
  // `Go` itself was an event: re-arm after it.
  cast(&h, Ev::ArmEvent(5));
  advance(3);
  advance(60);
  assert_eq!(
    events(&log),
    vec![
      "timeout event in Idle".to_string(),
      format!("timeout generic in {:?}", busy("x")),
    ],
    "the generic timeout survived the state change and cancelled the event timeout"
  );
}

#[test]
fn enter_action_runs_on_every_state_change() {
  let (_node, h, log) = start("statem-enter@test");
  cast(&h, Ev::Go(busy("a")));
  cast(&h, Ev::Go(busy("a")));
  cast(&h, Ev::Stay);
  cast(&h, Ev::Go(busy("b")));
  cast(&h, Ev::Go(St::Idle));
  assert_eq!(
    logged(&log),
    vec![
      "enter None -> Idle".to_string(),
      format!("enter Some(Idle) -> {:?}", busy("a")),
      format!("enter Some({:?}) -> {:?}", busy("a"), busy("b")),
      format!("enter Some({:?}) -> Idle", busy("b")),
    ]
  );
}

#[test]
fn next_event_runs_before_the_mailbox() {
  let (_node, h, log) = start("statem-next-event@test");
  cast(&h, Ev::Go(busy("x")));
  cast(&h, Ev::Work(1));
  cast(&h, Ev::Chain);
  assert_eq!(
    events(&log),
    vec!["work 99 (internal)", "work 1 (mailbox)"],
    "inserted before the re-delivered postponed event"
  );
}

#[test]
fn stop_exits_with_its_reason() {
  let (node, h, _log) = start("statem-stop@test");
  let exits = watch_exit(&node, h.pid());
  cast(&h, Ev::Crash);
  assert_eq!(exits.try_recv(), Ok(ExitReason::error("crash requested")));
  assert!(matches!(
    run(h.call(Ev::Ask(false), T)),
    Err(CallError::MailboxClosed | CallError::Down(_))
  ));
}

#[test]
fn statem_under_supervisor_restarts() {
  let node = node("statem-sup@test");
  let log = Log::default();
  let current: Arc<Mutex<Option<Handle>>> = Arc::default();
  let publish = Arc::clone(&current);
  let args = Arc::clone(&log);
  let start: StartFn = Arc::new(move |node, sup| {
    let addr = node.spawn_linked_to::<StatemProcess<Machine>>(sup, Arc::clone(&args), ());
    let pid = addr.pid().clone();
    *publish.lock().unwrap() = Some(StatemHandle::from_addr(node, addr));
    Ok(pid)
  });
  let tree = SupervisorSpec::new(Strategy::OneForOne)
    .child(ChildSpec::from_start("machine", start))
    .start(&node);
  settle();
  run(tree.which_children(T)).unwrap();
  let first = current.lock().unwrap().clone().unwrap();
  cast(&first, Ev::Go(busy("x")));
  cast(&first, Ev::ArmState(5));
  let exits = watch_exit(&node, first.pid());
  cast(&first, Ev::Crash);
  assert_eq!(exits.try_recv(), Ok(ExitReason::error("crash requested")));

  let kids = run(tree.which_children(T)).unwrap();
  let second = current.lock().unwrap().clone().unwrap();
  assert_eq!(kids[0].1.as_ref(), Some(second.pid()));
  assert_ne!(first.pid(), second.pid());
  assert_eq!(run(second.state(T)), Ok(St::Idle), "restarted from init");
  advance(60);
  assert!(
    events(&log).is_empty(),
    "the crashed incarnation's state timeout died with it"
  );
  let enters = logged(&log)
    .iter()
    .filter(|e| e.as_str() == "enter None -> Idle")
    .count();
  assert_eq!(enters, 2, "init ran the initial enter for each incarnation");
}
