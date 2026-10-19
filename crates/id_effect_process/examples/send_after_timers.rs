//! Timers — `send_after`, `cancel_timer` and the idle timeout.
//!
//! A ticker re-arms a 10 ms timer from its own `handle_info`; a caller outside the process
//! schedules a reminder with `Addr::send_after`; stopping the ticker cancels its pending timer
//! and sets an idle timeout, which delivers `Info::Timeout` once the process has been idle.
//!
//! `cargo run -p id_effect_process --example send_after_timers`

use std::time::Duration;

use id_effect::{Effect, HNil, Never, sleep, succeed};
use id_effect_process::{Info, Next, NodeName, Process, ProcessCtx, ProcessNode, TimerRef};

const TICK: Duration = Duration::from_millis(10);

enum Msg {
  Tick,
  Reminder(&'static str),
}

enum Call {
  Ticks,
  /// Cancel the pending tick; replies 1 when the cancel stopped it.
  StopTicking,
  /// How many idle timeouts arrived.
  Timeouts,
}

struct Ticker {
  ticks: u32,
  timeouts: u32,
  next: Option<TimerRef>,
}

impl Process for Ticker {
  type Args = ();
  type Env = ();
  type Call = Call;
  type Reply = u32;
  type Cast = ();
  type Info = Msg;
  type Error = Never;

  fn init(_: (), ctx: ProcessCtx) -> Effect<Self, Never, ()> {
    let next = Some(ctx.send_after::<Self>(Msg::Tick, TICK));
    succeed(Ticker {
      ticks: 0,
      timeouts: 0,
      next,
    })
  }

  fn handle_call(mut self, msg: Call, ctx: ProcessCtx) -> Effect<(u32, Next<Self>), Never, ()> {
    let reply = match msg {
      Call::Ticks => self.ticks,
      Call::StopTicking => {
        // `None` from now on, so a tick that fired before the cancel does not re-arm.
        let stopped = self.next.take().is_some_and(|t| ctx.cancel_timer(t));
        ctx.set_idle_timeout(Duration::from_millis(30));
        u32::from(stopped)
      }
      Call::Timeouts => self.timeouts,
    };
    succeed((reply, Next::Continue(self)))
  }

  fn handle_info(mut self, msg: Info<Msg>, ctx: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    match msg {
      Info::User(Msg::Tick) if self.next.is_some() => {
        self.ticks += 1;
        self.next = Some(ctx.send_after::<Self>(Msg::Tick, TICK));
      }
      Info::User(Msg::Reminder(text)) => println!("reminder: {text}"),
      Info::Timeout => self.timeouts += 1,
      _ => {}
    }
    succeed(Next::Continue(self))
  }
}

fn ask(addr: &id_effect_process::Addr<Ticker>, msg: Call) -> u32 {
  addr
    .call::<HNil>(msg, Duration::from_secs(5))
    .block_on()
    .into_result()
    .expect("ticker answers")
}

fn pause(ms: u64) {
  let _ = sleep::<Never, HNil>(Duration::from_millis(ms)).block_on();
}

fn main() {
  let node = ProcessNode::new(NodeName::new("timers@local"));
  let ticker = node.spawn::<Ticker>((), ());
  let reminder = ticker.send_after(Msg::Reminder("from outside"), Duration::from_millis(25));

  pause(60);
  let stopped = ask(&ticker, Call::StopTicking) == 1;
  let ticks = ask(&ticker, Call::Ticks);
  println!("ticks before stop: {ticks}; pending tick cancelled: {stopped}");
  println!("reminder still pending: {}", reminder.is_pending());

  pause(100);
  let after = ask(&ticker, Call::Ticks);
  assert_eq!(after, ticks, "no tick after the cancel");
  let timeouts = ask(&ticker, Call::Timeouts);
  println!("ticks after stop: {after}; idle timeouts: {timeouts}");
  assert!(timeouts >= 1, "the idle ticker got Info::Timeout");
}
