//! Hibernate — shed memory while idle and resume on the next message.
//!
//! A burst queues behind a blocked handler and grows the mailbox buffer to its peak; draining
//! it keeps that allocation. `Next::Hibernate` runs the `Process::hibernate` hook (here it drops
//! a cache), releases the mailbox buffer, and the process resumes on the next message.
//!
//! `cargo run -p id_effect_process --example hibernate`

use std::time::Duration;

use id_effect::{Deferred, Effect, HNil, Never, succeed};
use id_effect_process::{Addr, Info, Next, NodeName, Process, ProcessCtx, ProcessNode};

const BURST: u32 = 5_000;

enum Call {
  Seen,
  Hibernate,
}

struct Counter {
  seen: u32,
  /// Scratch memory worth dropping before going idle.
  cache: Vec<u32>,
}

impl Process for Counter {
  type Args = ();
  type Env = ();
  type Call = Call;
  type Reply = u32;
  /// Blocks the loop until the gate completes, so the burst queues up.
  type Cast = Deferred<(), Never>;
  type Info = u32;
  type Error = Never;

  fn init(_: (), _: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Counter {
      seen: 0,
      cache: Vec::new(),
    })
  }

  fn handle_call(self, msg: Call, _: ProcessCtx) -> Effect<(u32, Next<Self>), Never, ()> {
    let seen = self.seen;
    match msg {
      Call::Seen => succeed((seen, Next::Continue(self))),
      Call::Hibernate => succeed((seen, Next::Hibernate(self))),
    }
  }

  fn handle_cast(self, gate: Deferred<(), Never>, _: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    gate.wait().map(move |()| Next::Continue(self))
  }

  fn handle_info(mut self, msg: Info<u32>, _: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    if let Info::User(n) = msg {
      self.seen += 1;
      self.cache.push(n);
    }
    succeed(Next::Continue(self))
  }

  fn hibernate(mut self, _: ProcessCtx) -> Effect<Self, Never, ()> {
    println!(
      "hibernate hook: dropping a cache of {} entries",
      self.cache.len()
    );
    self.cache = Vec::new();
    succeed(self)
  }
}

fn ask(addr: &Addr<Counter>, msg: Call) -> u32 {
  addr
    .call::<HNil>(msg, Duration::from_secs(5))
    .block_on()
    .into_result()
    .expect("counter answers")
}

fn main() {
  let node = ProcessNode::new(NodeName::new("hibernate@local"));
  let counter = node.spawn::<Counter>((), ());
  let mailbox = counter.clone_mailbox();

  let gate: Deferred<(), Never> = Deferred::new();
  counter
    .cast::<HNil>(gate.clone())
    .block_on()
    .into_result()
    .expect("cast");
  for n in 0..BURST {
    assert!(counter.send_now(n));
  }
  let peak = mailbox.allocated();
  gate.try_succeed(());

  // The burst is drained by the time this call is answered; the loop then hibernates.
  let seen = ask(&counter, Call::Hibernate);
  // Answered only once the hibernation is over and the loop resumed.
  assert_eq!(ask(&counter, Call::Seen), seen);
  println!(
    "seen {seen} messages; mailbox buffer {peak} slots at the peak, {} after hibernating",
    mailbox.allocated()
  );
  assert!(mailbox.allocated() < peak);

  assert!(counter.send_now(BURST));
  assert_eq!(
    ask(&counter, Call::Seen),
    BURST + 1,
    "resumed on the next message"
  );
}
