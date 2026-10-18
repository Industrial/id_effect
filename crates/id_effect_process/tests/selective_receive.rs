//! Selective receive (fault.actors #7): `ProcessCtx::receive(matcher, timeout)` takes the first
//! message the matcher accepts and leaves every skipped message queued, in arrival order, for
//! the normal loop.
//!
//! No sleeps: see `common` for the synchronization fixtures.

mod common;

use std::time::Duration;

use common::{JoinNode, T, advance, run, settle};
use id_effect::{Cause, Deferred, Effect, Exit, Never, succeed};
use id_effect_process::{
  Addr, CallError, Info, Next, ProcMsg, Process, ProcessCtx, ReceiveError, SpawnOptions,
};

/// Logs infos; `Pick(n, within)` selectively receives the info `n` (logged as `100 + n`) or
/// logs the receive error (`-1` timeout, `-2` scan limit, `-3` anything else).
struct Picker {
  log: Vec<i64>,
}

enum PickCall {
  Log,
  Pick(u32, Duration),
}

impl Process for Picker {
  type Args = ();
  type Env = ();
  type Call = PickCall;
  type Reply = Vec<i64>;
  type Cast = ();
  type Info = u32;
  type Error = Never;

  fn init(_args: (), _ctx: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Picker { log: Vec::new() })
  }

  fn handle_call(
    mut self,
    msg: PickCall,
    ctx: ProcessCtx,
  ) -> Effect<(Vec<i64>, Next<Self>), Never, ()> {
    match msg {
      PickCall::Log => succeed((self.log.clone(), Next::Continue(self))),
      PickCall::Pick(n, within) => ctx
        .receive::<Self, ()>(
          move |m| matches!(m, ProcMsg::Info(Info::User(x)) if *x == n),
          within,
        )
        .exit::<Never>()
        .map(move |exit| {
          let entry = match exit {
            Exit::Success(ProcMsg::Info(Info::User(x))) => 100 + i64::from(x),
            Exit::Failure(Cause::Fail(ReceiveError::Timeout)) => -1,
            Exit::Failure(Cause::Fail(ReceiveError::ScanLimit)) => -2,
            _ => -3,
          };
          self.log.push(entry);
          (self.log.clone(), Next::Continue(self))
        }),
    }
  }

  fn handle_info(mut self, msg: Info<u32>, _ctx: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    if let Info::User(x) = msg {
      self.log.push(i64::from(x));
    }
    succeed(Next::Continue(self))
  }
}

/// Enqueue a `Pick` call straight into the mailbox (before the process has run), so later
/// infos can be queued ahead of the scan; the returned reply resolves with the log.
fn enqueue_pick(addr: &Addr<Picker>, n: u32, within: Duration) -> Deferred<Vec<i64>, CallError> {
  let reply = Deferred::new();
  assert!(
    addr
      .clone_mailbox()
      .offer_now(ProcMsg::Call(PickCall::Pick(n, within), reply.clone()))
  );
  reply
}

fn log(addr: &Addr<Picker>) -> Vec<i64> {
  run(addr.call(PickCall::Log, T)).expect("log call")
}

#[test]
fn selective_receive_keeps_order_of_skipped_messages() {
  let jn = JoinNode::new("selective@test");
  let addr = jn.node.spawn::<Picker>((), ());
  let reply = enqueue_pick(&addr, 3, T);
  // Buffered before the scan starts.
  for n in [1, 2, 4] {
    assert!(addr.send_now(n));
  }
  settle();
  assert_eq!(
    addr.clone_mailbox().len(),
    3,
    "the receive is waiting; nothing was consumed"
  );
  // Arrive while the receive waits; `3` is taken from between `5` and `6`.
  for n in [5, 3, 6] {
    assert!(addr.send_now(n));
  }
  settle();
  assert_eq!(run(reply.wait()), Ok(vec![103]));
  assert_eq!(log(&addr), vec![103, 1, 2, 4, 5, 6]);
}

#[test]
fn selective_receive_times_out() {
  let jn = JoinNode::new("selective-timeout@test");
  let addr = jn.node.spawn::<Picker>((), ());
  let reply = enqueue_pick(&addr, 9, Duration::from_secs(5));
  assert!(addr.send_now(1));
  assert!(addr.send_now(2));
  settle();
  advance(Duration::from_secs(4));
  settle();
  assert_eq!(
    addr.clone_mailbox().len(),
    2,
    "still waiting before the deadline"
  );
  advance(Duration::from_secs(1));
  settle();
  assert_eq!(run(reply.wait()), Ok(vec![-1]));
  assert_eq!(
    log(&addr),
    vec![-1, 1, 2],
    "skipped messages survive the timeout"
  );
  assert_eq!(ReceiveError::Timeout.to_string(), "receive timed out");
}

#[test]
fn selective_receive_scan_is_bounded_by_capacity() {
  let jn = JoinNode::new("selective-bound@test");
  let addr = jn
    .node
    .spawn_with::<Picker>((), (), SpawnOptions::new().mailbox_capacity(3));
  let reply = enqueue_pick(&addr, 9, T);
  assert!(addr.send_now(1));
  assert!(addr.send_now(2));
  settle();
  assert!(addr.send_now(3));
  settle();
  assert_eq!(run(reply.wait()), Ok(vec![-2]));
  assert_eq!(log(&addr), vec![-2, 1, 2, 3]);
}
