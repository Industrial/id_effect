//! Timers, idle timeouts and hibernate (fault.actors #6, #8): `send_after` on the executor
//! clock, `cancel_timer`, timers dying with their process, `Info::Timeout` once per idle
//! period, and `Next::Hibernate` releasing the mailbox buffer.
//!
//! No sleeps: see `common` for the synchronization fixtures. The behavior logs into a shared
//! vector so that reading the log sends no message (a message would start a new idle period).

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{JoinNode, T, advance, await_exit, run, settle, watch_exit};
use id_effect::{Deferred, Effect, Never, succeed};
use id_effect_process::{
  Addr, ExitReason, Info, Next, Process, ProcessCtx, SpawnOptions, TimerRef,
};

type Log = Arc<Mutex<Vec<i64>>>;

/// Logs user infos as themselves, `Info::Timeout` as `-1` and each hibernation as `-2`.
struct Clock {
  log: Log,
}

enum ClockCall {
  SendAfter(u32, Duration),
  Cancel(TimerRef),
  Idle(Option<Duration>),
  Hibernate,
  Stop,
}

#[derive(Clone, Debug)]
enum ClockReply {
  Timer(TimerRef),
  Cancelled(bool),
  Done,
}

impl Process for Clock {
  type Args = Log;
  type Env = ();
  type Call = ClockCall;
  type Reply = ClockReply;
  /// Blocks the loop until the gate completes (keeps later messages queued).
  type Cast = Deferred<(), Never>;
  type Info = u32;
  type Error = Never;

  fn init(log: Log, _ctx: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Clock { log })
  }

  fn handle_call(
    self,
    msg: ClockCall,
    ctx: ProcessCtx,
  ) -> Effect<(ClockReply, Next<Self>), Never, ()> {
    let reply = match msg {
      ClockCall::SendAfter(n, after) => ClockReply::Timer(ctx.send_after::<Self>(n, after)),
      ClockCall::Cancel(timer) => ClockReply::Cancelled(ctx.cancel_timer(timer)),
      ClockCall::Idle(Some(after)) => {
        ctx.set_idle_timeout(after);
        ClockReply::Done
      }
      ClockCall::Idle(None) => {
        ctx.clear_idle_timeout();
        ClockReply::Done
      }
      ClockCall::Hibernate => return succeed((ClockReply::Done, Next::Hibernate(self))),
      ClockCall::Stop => {
        return succeed((ClockReply::Done, Next::Stop(ExitReason::Normal, self)));
      }
    };
    succeed((reply, Next::Continue(self)))
  }

  fn handle_cast(
    self,
    gate: Deferred<(), Never>,
    _ctx: ProcessCtx,
  ) -> Effect<Next<Self>, Never, ()> {
    gate.wait().map(move |()| Next::Continue(self))
  }

  fn handle_info(self, msg: Info<u32>, _ctx: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    match msg {
      Info::User(n) => self.push(i64::from(n)),
      Info::Timeout => self.push(-1),
      _ => {}
    }
    succeed(Next::Continue(self))
  }

  fn hibernate(self, _ctx: ProcessCtx) -> Effect<Self, Never, ()> {
    self.push(-2);
    succeed(self)
  }
}

impl Clock {
  fn push(&self, entry: i64) {
    self.log.lock().expect("log mutex").push(entry);
  }
}

fn spawn(jn: &JoinNode, opts: SpawnOptions) -> (Addr<Clock>, Log) {
  let log = Log::default();
  let addr = jn.node.spawn_with::<Clock>(Arc::clone(&log), (), opts);
  settle();
  (addr, log)
}

fn logged(log: &Log) -> Vec<i64> {
  log.lock().expect("log mutex").clone()
}

fn call(addr: &Addr<Clock>, msg: ClockCall) -> ClockReply {
  run(addr.call(msg, T)).expect("clock call")
}

fn timer(addr: &Addr<Clock>, n: u32, after: Duration) -> TimerRef {
  match call(addr, ClockCall::SendAfter(n, after)) {
    ClockReply::Timer(t) => t,
    other => panic!("expected a timer, got {other:?}"),
  }
}

fn cancel(addr: &Addr<Clock>, t: &TimerRef) -> bool {
  match call(addr, ClockCall::Cancel(t.clone())) {
    ClockReply::Cancelled(won) => won,
    other => panic!("expected a cancel result, got {other:?}"),
  }
}

fn secs(n: u64) -> Duration {
  Duration::from_secs(n)
}

#[test]
fn send_after_delivers_under_test_clock() {
  let jn = JoinNode::new("timers-deliver@test");
  let (addr, log) = spawn(&jn, SpawnOptions::new());
  let inside = timer(&addr, 7, secs(5));
  let outside = addr.send_after(8, secs(2));
  assert!(inside.is_pending() && outside.is_pending());

  advance(secs(1));
  settle();
  assert_eq!(logged(&log), Vec::<i64>::new(), "nothing is due yet");
  advance(secs(1));
  settle();
  assert_eq!(logged(&log), vec![8], "Addr::send_after fired at 2 s");
  advance(secs(2));
  settle();
  assert_eq!(
    logged(&log),
    vec![8],
    "one virtual second before the 5 s timer"
  );
  advance(secs(1));
  settle();
  assert_eq!(logged(&log), vec![8, 7]);
  assert!(!inside.is_pending() && !outside.is_pending());
  assert!(!inside.cancel(), "a fired timer cannot be cancelled");
}

#[test]
fn cancelled_timer_never_delivers() {
  let jn = JoinNode::new("timers-cancel@test");
  let (addr, log) = spawn(&jn, SpawnOptions::new());
  let inside = timer(&addr, 1, secs(5));
  let outside = addr.send_after(2, secs(5));
  let kept = addr.send_after(3, secs(5));

  advance(secs(4));
  settle();
  assert!(
    cancel(&addr, &inside),
    "ProcessCtx::cancel_timer stops a pending timer"
  );
  assert!(
    outside.cancel(),
    "TimerRef::cancel works from outside the process"
  );
  assert!(
    !inside.cancel() && !outside.cancel(),
    "a second cancel reports false"
  );

  advance(secs(60));
  settle();
  assert_eq!(
    logged(&log),
    vec![3],
    "only the uncancelled timer delivered"
  );
  assert!(!kept.cancel());
}

#[test]
fn timers_die_with_process() {
  let jn = JoinNode::new("timers-die@test");
  let (addr, log) = spawn(&jn, SpawnOptions::new());
  let inside = timer(&addr, 1, secs(5));
  let outside = addr.send_after(2, secs(5));
  let exit = watch_exit(&jn.node, addr.pid());

  call(&addr, ClockCall::Stop);
  assert_eq!(await_exit(&exit), ExitReason::Normal);
  assert!(
    !inside.is_pending() && !outside.is_pending(),
    "exit cancelled both"
  );
  assert!(
    !inside.cancel() && !outside.cancel(),
    "nothing left to cancel"
  );

  advance(secs(60));
  settle();
  assert_eq!(logged(&log), Vec::<i64>::new());
  let late = addr.send_after(3, secs(1));
  assert!(!late.is_pending(), "a timer to a dead process is inert");
}

#[test]
fn timer_into_full_mailbox_counts_against_bound() {
  let jn = JoinNode::new("timers-full@test");
  let (addr, log) = spawn(&jn, SpawnOptions::new().mailbox_capacity(1));
  let fired = addr.send_after(9, secs(1));
  // The loop blocks on the gate, so the one-slot mailbox stays full.
  let gate: Deferred<(), Never> = Deferred::new();
  run(addr.cast(gate.clone())).expect("cast");
  settle();
  assert!(addr.send_now(5), "fills the single slot");
  assert!(!addr.send_now(6), "the mailbox is full");

  advance(secs(1));
  settle();
  assert!(!fired.is_pending(), "the timer fired");
  gate.try_succeed(());
  settle();
  assert_eq!(
    logged(&log),
    vec![5],
    "a user-message timer is dropped by a full mailbox (D17)"
  );
}

#[test]
fn idle_timeout_fires_once_per_idle_period() {
  let jn = JoinNode::new("timers-idle@test");
  let (addr, log) = spawn(&jn, SpawnOptions::new());
  call(&addr, ClockCall::Idle(Some(secs(3))));

  advance(secs(2));
  settle();
  assert_eq!(logged(&log), Vec::<i64>::new());
  advance(secs(1));
  settle();
  assert_eq!(logged(&log), vec![-1], "Info::Timeout after 3 idle seconds");
  advance(secs(30));
  settle();
  assert_eq!(logged(&log), vec![-1], "once per idle period");

  assert!(addr.send_now(5));
  settle();
  advance(secs(2));
  settle();
  assert_eq!(
    logged(&log),
    vec![-1, 5],
    "a message re-arms the idle timeout"
  );
  advance(secs(1));
  settle();
  assert_eq!(logged(&log), vec![-1, 5, -1]);

  call(&addr, ClockCall::Idle(None));
  advance(secs(30));
  settle();
  assert_eq!(logged(&log), vec![-1, 5, -1], "cleared");
}

#[test]
fn hibernate_releases_buffer_and_resumes() {
  let jn = JoinNode::new("timers-hibernate@test");
  let (addr, log) = spawn(&jn, SpawnOptions::new());
  let mailbox = addr.clone_mailbox();

  // A burst while the loop is blocked grows the buffer to its peak.
  let gate: Deferred<(), Never> = Deferred::new();
  run(addr.cast(gate.clone())).expect("cast");
  settle();
  for n in 0..1_000 {
    assert!(addr.send_now(n));
  }
  gate.try_succeed(());
  settle();
  assert!(mailbox.is_empty());
  assert!(
    mailbox.allocated() >= 1_000,
    "draining keeps the peak allocation"
  );

  call(&addr, ClockCall::Hibernate);
  assert_eq!(logged(&log).last(), Some(&-2), "the hibernate hook ran");
  assert!(
    mailbox.allocated() < 1_000,
    "hibernating released the buffer"
  );
  assert_eq!(mailbox.allocated(), 0, "an empty mailbox keeps no buffer");

  assert!(addr.send_now(42));
  settle();
  assert_eq!(
    logged(&log).last(),
    Some(&42),
    "resumed on the next message"
  );
  assert!(addr.is_alive());
}
