//! Bounded mailboxes (M10, decision D17): every mailbox holds at most its capacity of user
//! messages, a send to a full mailbox fails instead of queueing, and system messages (exit
//! signals, monitor downs, kill) bypass the bound.
//!
//! No sleeps: see `common` for the synchronization fixtures.

mod common;

use common::{JoinNode, Recorder, T, await_exit, run, settle, watch_exit};
use id_effect::{Deferred, Effect, Never, succeed};
use id_effect_process::{
  CallError, DEFAULT_MAILBOX_CAPACITY, ExitReason, Info, Mailbox, Next, OfferError, Process,
  ProcessCtx, SendError, SpawnOptions,
};

/// Logs casts and infos; the cast `0` blocks its handler until the gate opens, so the test can
/// fill the mailbox deterministically. Traps exits.
struct Gated {
  gate: Option<Deferred<(), Never>>,
  log: Vec<i64>,
}

impl Process for Gated {
  type Args = Deferred<(), Never>;
  type Env = ();
  type Call = ();
  type Reply = Vec<i64>;
  type Cast = i64;
  type Info = i64;
  type Error = Never;

  fn init(gate: Deferred<(), Never>, ctx: ProcessCtx) -> Effect<Self, Never, ()> {
    ctx.trap_exit(true);
    succeed(Gated {
      gate: Some(gate),
      log: Vec::new(),
    })
  }

  fn handle_call(self, _msg: (), _ctx: ProcessCtx) -> Effect<(Vec<i64>, Next<Self>), Never, ()> {
    succeed((self.log.clone(), Next::Continue(self)))
  }

  fn handle_cast(mut self, n: i64, _ctx: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    if n != 0 {
      self.log.push(n);
      return succeed(Next::Continue(self));
    }
    match self.gate.take() {
      Some(gate) => gate.wait().map(move |()| Next::Continue(self)),
      None => succeed(Next::Continue(self)),
    }
  }

  fn handle_info(mut self, msg: Info<i64>, _ctx: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    match msg {
      Info::User(n) => self.log.push(n),
      Info::Down { .. } => self.log.push(-1),
      Info::Exit { .. } => self.log.push(-2),
    }
    succeed(Next::Continue(self))
  }
}

/// A gated process bounded at `capacity`, already blocked in its handler (mailbox empty).
fn blocked(
  jn: &JoinNode,
  capacity: usize,
) -> (id_effect_process::Addr<Gated>, Deferred<(), Never>) {
  let gate = Deferred::new();
  let addr = jn.node.spawn_with::<Gated>(
    gate.clone(),
    (),
    SpawnOptions::new().mailbox_capacity(capacity),
  );
  run(addr.cast(0)).expect("the blocking cast");
  assert_eq!(
    addr.clone_mailbox().len(),
    0,
    "the handler took the blocking cast"
  );
  (addr, gate)
}

#[test]
fn m10_bounded_mailbox_rejects_when_full() {
  let jn = JoinNode::new("bounded@test");
  assert_eq!(DEFAULT_MAILBOX_CAPACITY, 10_000);
  let default = jn.node.spawn::<Recorder>(None, ());
  assert_eq!(default.clone_mailbox().capacity(), DEFAULT_MAILBOX_CAPACITY);

  let (addr, gate) = blocked(&jn, 3);
  assert_eq!(addr.clone_mailbox().capacity(), 3);
  for n in 1..=3 {
    run(addr.cast(n)).expect("room for three");
  }
  assert_eq!(run(addr.cast(4)), Err(SendError::Full));
  assert_eq!(run(addr.send(5)), Err(SendError::Full));
  assert!(!addr.send_now(6), "send_now reports a full mailbox");
  assert_eq!(run(addr.call((), T)), Err(CallError::MailboxFull));
  assert_eq!(
    addr.clone_mailbox().len(),
    3,
    "nothing past the bound was queued"
  );

  assert!(gate.try_succeed(()));
  settle();
  assert_eq!(run(addr.call((), T)), Ok(vec![1, 2, 3]));
  run(addr.cast(7)).expect("a drained mailbox has room again");
  assert_eq!(run(addr.call((), T)), Ok(vec![1, 2, 3, 7]));

  assert_eq!(
    SendError::Full.to_string(),
    "message rejected: mailbox full"
  );
  assert_eq!(CallError::MailboxFull.to_string(), "mailbox full");
}

#[test]
fn m10_bounded_mailbox_offers_report_full_and_closed() {
  let mb: Mailbox<u8> = Mailbox::with_capacity(0);
  assert_eq!(mb.capacity(), 1, "a capacity is at least one");
  assert_eq!(mb.try_offer_now(1), Ok(()));
  assert_eq!(mb.try_offer_now(2), Err(OfferError::Full));
  assert!(!mb.offer_now(3));
  assert!(mb.offer_system_now(4), "system messages bypass the bound");
  assert_eq!(mb.len(), 2);
  mb.close();
  assert_eq!(mb.try_offer_now(5), Err(OfferError::Closed));
  assert!(
    !mb.offer_system_now(6),
    "a closed mailbox refuses system messages too"
  );
  assert_eq!(OfferError::Full.to_string(), "mailbox full");
  assert_eq!(OfferError::Closed.to_string(), "mailbox closed");
  assert_eq!(SpawnOptions::new().mailbox_capacity(0).capacity(), 1);
  assert_eq!(SpawnOptions::default().capacity(), DEFAULT_MAILBOX_CAPACITY);
}

#[test]
fn m10_system_messages_bypass_bound() {
  let jn = JoinNode::new("sysbypass@test");
  let (addr, gate) = blocked(&jn, 1);
  run(addr.cast(1)).expect("fills the mailbox");
  assert_eq!(run(addr.cast(2)), Err(SendError::Full));

  // Monitor downs and a trapped link exit arrive although the mailbox is full.
  let watched: Vec<_> = (0..3)
    .map(|_| jn.node.spawn::<Recorder>(None, ()))
    .collect();
  for w in &watched {
    jn.node.monitor_into(addr.pid(), w.pid());
  }
  let linked = jn.node.spawn::<Recorder>(None, ());
  jn.node.link(addr.pid(), linked.pid());
  for w in &watched {
    jn.node.exit(w.pid(), ExitReason::Killed);
  }
  jn.node.exit(linked.pid(), ExitReason::Killed);
  settle();
  assert_eq!(
    addr.clone_mailbox().len(),
    5,
    "one user message, three downs and one exit signal"
  );
  assert_eq!(
    run(addr.cast(2)),
    Err(SendError::Full),
    "system messages take no user room"
  );

  assert!(gate.try_succeed(()));
  settle();
  assert_eq!(run(addr.call((), T)), Ok(vec![1, -1, -1, -1, -2]));

  // A kill reaches a process whose mailbox is full.
  let (victim, gate) = blocked(&jn, 1);
  run(victim.cast(1)).expect("fills the mailbox");
  let exit = watch_exit(&jn.node, victim.pid());
  jn.node.exit(victim.pid(), ExitReason::Killed);
  assert!(gate.try_succeed(()));
  assert_eq!(await_exit(&exit), ExitReason::Killed);
}
