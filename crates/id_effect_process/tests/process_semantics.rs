//! Phase 1 semantics tests: spawn/call/cast/info, links, monitors,
//! trap_exit, and the exactly-once monitor guarantee (ADR 0009 tables).

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::{JoinNode, advance, run, settle};
use id_effect::Never;
use id_effect::{Effect, fail, sleep, succeed, suspend, sync};
use id_effect_process::{
  Addr, CallError, ExitReason, Info, Next, Process, ProcessCtx, ProcessNode,
};

/// A node on this test's `TestExecutor` (virtual time, driven by `common`).
fn node() -> ProcessNode {
  JoinNode::new("test@local").node
}

/// Run the executor until it stalls, then evaluate `check`.
fn settled(check: impl FnOnce() -> bool) -> bool {
  settle();
  check()
}

// ── Counter behavior ────────────────────────────────────────

struct Counter {
  value: i64,
}

enum CounterCall {
  Get,
  Add(i64),
  Crash,
  StopGracefully,
}

impl Process for Counter {
  type Args = i64;
  type Env = ();
  type Call = CounterCall;
  type Reply = i64;
  type Cast = i64;
  type Info = i64;
  type Error = String;

  fn init(start: i64, _ctx: ProcessCtx) -> Effect<Self, String, ()> {
    succeed(Counter { value: start })
  }

  fn handle_call(
    mut self,
    msg: CounterCall,
    _ctx: ProcessCtx,
  ) -> Effect<(i64, Next<Self>), String, ()> {
    suspend(move || match msg {
      CounterCall::Get => succeed((self.value, Next::Continue(self))),
      CounterCall::Add(n) => {
        self.value += n;
        succeed((self.value, Next::Continue(self)))
      }
      CounterCall::Crash => fail("deliberate crash".to_string()),
      CounterCall::StopGracefully => {
        let v = self.value;
        succeed((v, Next::Stop(ExitReason::Normal, self)))
      }
    })
  }

  fn handle_cast(mut self, n: i64, _ctx: ProcessCtx) -> Effect<Next<Self>, String, ()> {
    sync(move || {
      self.value += n;
      Next::Continue(self)
    })
  }

  fn handle_info(mut self, msg: Info<i64>, _ctx: ProcessCtx) -> Effect<Next<Self>, String, ()> {
    sync(move || {
      if let Info::User(n) = msg {
        self.value += n;
      }
      Next::Continue(self)
    })
  }
}

fn call(addr: &Addr<Counter>, msg: CounterCall) -> Result<i64, CallError> {
  run(addr.call(msg, Duration::from_secs(5)))
}

#[test]
fn call_roundtrips_state() {
  let node = node();
  let addr = node.spawn::<Counter>(10, ());
  assert_eq!(call(&addr, CounterCall::Add(5)), Ok(15));
  assert_eq!(call(&addr, CounterCall::Get), Ok(15));
}

#[test]
fn cast_and_info_are_ordered_per_sender() {
  let node = node();
  let addr = node.spawn::<Counter>(0, ());
  run(addr.cast(1)).unwrap();
  run(addr.send(2)).unwrap();
  run(addr.cast(3)).unwrap();
  assert_eq!(call(&addr, CounterCall::Get), Ok(6));
}

#[test]
fn crash_fails_pending_call_with_down() {
  let node = node();
  let addr = node.spawn::<Counter>(0, ());
  let out = call(&addr, CounterCall::Crash);
  match out {
    Err(CallError::Down(ExitReason::Error(msg))) => {
      assert!(msg.contains("deliberate crash"), "got: {msg}");
    }
    other => panic!("expected Down(Error), got {other:?}"),
  }
  assert!(settled(|| !addr.is_alive()));
}

#[test]
fn graceful_stop_replies_then_exits_normal() {
  let node = node();
  let addr = node.spawn::<Counter>(7, ());
  assert_eq!(call(&addr, CounterCall::StopGracefully), Ok(7));
  assert!(settled(|| !addr.is_alive()));
}

#[test]
fn call_after_death_fails_fast() {
  let node = node();
  let addr = node.spawn::<Counter>(0, ());
  let _ = call(&addr, CounterCall::StopGracefully);
  assert!(settled(|| !addr.is_alive()));
  let out = call(&addr, CounterCall::Get);
  assert!(
    matches!(
      out,
      Err(CallError::MailboxClosed) | Err(CallError::Down(ExitReason::NoProc))
    ),
    "got {out:?}"
  );
}

#[test]
fn exit_kill_is_untrappable_even_for_trapping_process() {
  let node = node();
  let addr = node.spawn::<Trapper>(TrapperArgs::default(), ());
  // Wait for init (which sets trap_exit).
  let _ = call_trapper(&addr, TrapperCall::SeenExits);
  node.exit(addr.pid(), ExitReason::Killed);
  assert!(settled(|| !addr.is_alive()));
}

// ── Link / trap_exit semantics ──────────────────────────────────

#[derive(Default)]
struct TrapperArgs {
  link_to: Option<id_effect_process::Pid>,
  trap: Option<bool>,
}

struct Trapper {
  seen_exits: Vec<(id_effect_process::Pid, ExitReason)>,
  seen_downs: Vec<(id_effect_process::Pid, ExitReason)>,
}

enum TrapperCall {
  SeenExits,
  SeenDowns,
  Monitor(id_effect_process::Pid),
}

impl Process for Trapper {
  type Args = TrapperArgs;
  type Env = ();
  type Call = TrapperCall;
  type Reply = usize;
  type Cast = ();
  type Info = ();
  type Error = Never;

  fn init(args: TrapperArgs, ctx: ProcessCtx) -> Effect<Self, Never, ()> {
    sync(move || {
      ctx.trap_exit(args.trap.unwrap_or(true));
      if let Some(pid) = args.link_to {
        ctx.link(&pid);
      }
      Trapper {
        seen_exits: Vec::new(),
        seen_downs: Vec::new(),
      }
    })
  }

  fn handle_call(
    self,
    msg: TrapperCall,
    ctx: ProcessCtx,
  ) -> Effect<(usize, Next<Self>), Never, ()> {
    sync(move || {
      let reply = match msg {
        TrapperCall::SeenExits => self.seen_exits.len(),
        TrapperCall::SeenDowns => self.seen_downs.len(),
        TrapperCall::Monitor(pid) => {
          ctx.monitor(&pid);
          0
        }
      };
      (reply, Next::Continue(self))
    })
  }

  fn handle_info(mut self, msg: Info<()>, _ctx: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    sync(move || {
      match msg {
        Info::Exit { from, reason } => self.seen_exits.push((from, reason)),
        Info::Down { pid, reason, .. } => self.seen_downs.push((pid, reason)),
        Info::User(()) => {}
      }
      Next::Continue(self)
    })
  }
}

fn call_trapper(addr: &Addr<Trapper>, msg: TrapperCall) -> Result<usize, CallError> {
  run(addr.call(msg, Duration::from_secs(5)))
}

#[test]
fn link_cascade_kills_non_trapping_peer() {
  let node = node();
  let victim = node.spawn::<Trapper>(
    TrapperArgs {
      link_to: None,
      trap: Some(false),
    },
    (),
  );
  let _ = call_trapper(&victim, TrapperCall::SeenExits);
  let crasher = node.spawn::<Counter>(0, ());
  node.link(victim.pid(), crasher.pid());
  let _ = call(&crasher, CounterCall::Crash);
  assert!(
    settled(|| !victim.is_alive()),
    "non-trapping linked process must die with the crasher"
  );
}

#[test]
fn trapping_peer_receives_exit_message_and_survives() {
  let node = node();
  let trapper = node.spawn::<Trapper>(TrapperArgs::default(), ());
  let _ = call_trapper(&trapper, TrapperCall::SeenExits);
  let crasher = node.spawn::<Counter>(0, ());
  node.link(trapper.pid(), crasher.pid());
  let _ = call(&crasher, CounterCall::Crash);
  assert!(settled(|| {
    call_trapper(&trapper, TrapperCall::SeenExits) == Ok(1)
  }));
  assert!(trapper.is_alive());
}

#[test]
fn normal_exit_does_not_kill_linked_peer() {
  let node = node();
  let peer = node.spawn::<Trapper>(
    TrapperArgs {
      link_to: None,
      trap: Some(false),
    },
    (),
  );
  let _ = call_trapper(&peer, TrapperCall::SeenExits);
  let stopper = node.spawn::<Counter>(0, ());
  node.link(peer.pid(), stopper.pid());
  let _ = call(&stopper, CounterCall::StopGracefully);
  assert!(settled(|| !stopper.is_alive()));
  assert!(peer.is_alive(), "normal exits must not cascade");
}

#[test]
fn link_to_dead_process_delivers_noproc() {
  let node = node();
  let dead = node.spawn::<Counter>(0, ());
  let _ = call(&dead, CounterCall::StopGracefully);
  assert!(settled(|| !dead.is_alive()));

  let trapper = node.spawn::<Trapper>(
    TrapperArgs {
      link_to: Some(dead.pid().clone()),
      trap: Some(true),
    },
    (),
  );
  assert!(settled(|| {
    call_trapper(&trapper, TrapperCall::SeenExits) == Ok(1)
  }));
}

// ── Monitor semantics ─────────────────────────────────────────

#[test]
fn monitor_fires_exactly_once_on_crash() {
  let node = node();
  let watcher = node.spawn::<Trapper>(TrapperArgs::default(), ());
  let target = node.spawn::<Counter>(0, ());
  let _ = call_trapper(&watcher, TrapperCall::Monitor(target.pid().clone()));
  let _ = call(&target, CounterCall::Crash);
  assert!(settled(|| {
    call_trapper(&watcher, TrapperCall::SeenDowns) == Ok(1)
  }));
  // No duplicate delivery after settling.
  settle();
  assert_eq!(call_trapper(&watcher, TrapperCall::SeenDowns), Ok(1));
}

#[test]
fn monitor_on_dead_process_fires_noproc_immediately() {
  let node = node();
  let watcher = node.spawn::<Trapper>(TrapperArgs::default(), ());
  let target = node.spawn::<Counter>(0, ());
  let _ = call(&target, CounterCall::StopGracefully);
  assert!(settled(|| !target.is_alive()));
  let _ = call_trapper(&watcher, TrapperCall::Monitor(target.pid().clone()));
  assert!(settled(|| {
    call_trapper(&watcher, TrapperCall::SeenDowns) == Ok(1)
  }));
}

// ── Call timeout ──────────────────────────────────────────────

struct Sleeper;

impl Process for Sleeper {
  type Args = ();
  type Env = ();
  type Call = u64;
  type Reply = ();
  type Cast = ();
  type Info = ();
  type Error = Never;

  fn init(_: (), _ctx: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Sleeper)
  }

  fn handle_call(self, sleep_ms: u64, _ctx: ProcessCtx) -> Effect<((), Next<Self>), Never, ()> {
    sleep(Duration::from_millis(sleep_ms)).map(move |()| ((), Next::Continue(self)))
  }
}

#[test]
fn call_times_out_when_handler_is_slow() {
  let node = node();
  let addr = node.spawn::<Sleeper>((), ());
  let call = addr
    .call(500, Duration::from_millis(50))
    .fork_root(Arc::clone(node.runtime().executor()));
  settle();
  assert!(!call.is_done(), "nothing elapses until virtual time moves");
  advance(Duration::from_millis(50));
  assert!(call.is_done(), "timed out at virtual 50 ms");
  assert_eq!(run(call.join()), Err(CallError::Timeout));
}

// ── Registry ──────────────────────────────────────────────────

#[test]
fn registry_registers_resolves_and_cleans_up_on_exit() {
  let node = node();
  let addr = node.spawn::<Counter>(1, ());
  assert!(node.register("counter", &addr));
  assert!(!node.register("counter", &addr), "names are unique");
  assert_eq!(node.whereis("counter"), Some(addr.pid().clone()));

  let resolved = node
    .whereis_addr::<Counter>("counter")
    .expect("typed lookup");
  assert_eq!(call(&resolved, CounterCall::Get), Ok(1));

  let _ = call(&addr, CounterCall::StopGracefully);
  assert!(settled(|| node.whereis("counter").is_none()));
}

// ── terminate hook ────────────────────────────────────────────

struct Cleanup {
  witness: Arc<AtomicU32>,
}

impl Process for Cleanup {
  type Args = Arc<AtomicU32>;
  type Env = ();
  type Call = ();
  type Reply = ();
  type Cast = ();
  type Info = ();
  type Error = Never;

  fn init(witness: Arc<AtomicU32>, _ctx: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Cleanup { witness })
  }

  fn handle_call(self, _: (), _ctx: ProcessCtx) -> Effect<((), Next<Self>), Never, ()> {
    succeed(((), Next::Continue(self)))
  }

  fn terminate(self, _reason: &ExitReason, _ctx: ProcessCtx) -> Effect<(), Never, ()> {
    sync(move || {
      self.witness.fetch_add(1, Ordering::SeqCst);
    })
  }
}

#[test]
fn terminate_runs_once_on_cooperative_kill() {
  let node = node();
  let witness = Arc::new(AtomicU32::new(0));
  let addr = node.spawn::<Cleanup>(Arc::clone(&witness), ());
  let _ = run(addr.call((), Duration::from_secs(5)));
  node.exit(addr.pid(), ExitReason::Shutdown);
  assert!(settled(|| !addr.is_alive()));
  assert!(settled(|| witness.load(Ordering::SeqCst) == 1));
  settle();
  assert_eq!(witness.load(Ordering::SeqCst), 1);
}
