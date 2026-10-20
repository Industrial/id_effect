//! [`Statem`] — `gen_statem` parity: states that carry data, a separate machine `Data`,
//! state/event/generic timeouts, postpone, inserted events, replies and state-enter callbacks.
//!
//! Unlike the table-driven [`FsmProcess`](crate::FsmProcess) (whose states are `Copy` keys of a
//! [`TransitionTable`](crate::TransitionTable)), a [`Statem`] is a callback module in the
//! `handle_event_function` style: [`Statem::handle_event`] sees the event, the current state
//! and the data, and returns a [`Transition`] with a list of [`Action`]s. It runs as a
//! [`StatemProcess`], an ordinary [`Process`], so it is spawned, linked, monitored and
//! supervised like any other process.
//!
//! ## Semantics (as in Erlang's `gen_statem`)
//!
//! - **State change**: a [`Transition::Next`] whose state differs (`!=`) from the current one.
//!   [`Transition::Keep`] and a `Next` to an equal state are not state changes.
//! - **State timeout** ([`Action::StateTimeout`]): one per machine; arming replaces the previous
//!   one; cancelled by a state change. A `StateTimeout` returned with a state change arms the
//!   new state's timeout.
//! - **Event timeout** ([`Action::EventTimeout`]): cancelled by any event, including inserted
//!   and re-delivered ones.
//! - **Generic timeouts** ([`Action::GenericTimeout`]): named; arming a name replaces its timer;
//!   never cancelled automatically ([`Action::CancelGenericTimeout`]).
//! - **Postpone** ([`Action::Postpone`]): the current event is kept aside and re-delivered after
//!   the next state change, in arrival order.
//! - **Inserted events** ([`Action::NextEvent`]): handled as [`StatemEvent::Internal`] before
//!   any postponed or mailbox event.
//! - **State enter** ([`Statem::on_enter`]): runs for the initial state and after every state
//!   change, before the next event. Its actions may arm or cancel timeouts and reply; a
//!   `Postpone` or `NextEvent` there (or in [`Statem::init`]) crashes the process, as in Erlang.
//!
//! A timer that was replaced or cancelled after its message reached the mailbox is recognised
//! and dropped, so a cancelled timeout is never delivered. Timers run on the node's executor
//! clock and die with the process.

use std::collections::{HashMap, VecDeque};
use std::fmt::{self, Debug};
use std::time::Duration;

use id_effect::{Cause, Deferred, Effect, fail, succeed, suspend, sync};
use id_effect_process::{
  Addr, CallError, ExitReason, Info, Next, Pid, Process, ProcessCtx, ProcessNode, SendError,
  TimerRef,
};

/// A `gen_statem`-style callback module. The implementing type is never instantiated: it only
/// names the machine's types and callbacks. Run it with [`StatemProcess::start`].
pub trait Statem: Sized + 'static {
  /// Arguments passed to [`Statem::init`].
  type Args: Send + 'static;
  /// Effect environment for the callbacks (provided once at spawn).
  type Env: Send + Sync + 'static;
  /// The state. May carry data (no `Copy` bound); compared with `!=` to detect state changes.
  type State: Clone + PartialEq + Debug + Send + Sync + 'static;
  /// The machine data travelling alongside the state.
  type Data: Send + 'static;
  /// Event payload: calls, casts, infos, timeout messages and inserted events.
  type Event: Send + 'static;
  /// Reply to a [`StatemHandle::call`].
  type Reply: Clone + Send + Sync + 'static;
  /// Callback error; a failing callback crashes the process with [`ExitReason::Error`].
  type Error: Debug + Clone + Send + Sync + 'static;

  /// The initial state, data and actions. Runs inside the new process.
  fn init(args: Self::Args, ctx: ProcessCtx) -> Effect<StatemInit<Self>, Self::Error, Self::Env>;

  /// Handle one event in `state` with `data`.
  fn handle_event(
    event: &StatemEvent<Self>,
    state: &Self::State,
    data: Self::Data,
    ctx: ProcessCtx,
  ) -> Effect<Transition<Self>, Self::Error, Self::Env>;

  /// State-enter callback: runs with `from = None` for the initial state and with the previous
  /// state after every state change. Default: no actions.
  fn on_enter(
    from: Option<&Self::State>,
    state: &Self::State,
    data: Self::Data,
    ctx: ProcessCtx,
  ) -> Effect<Entered<Self>, Self::Error, Self::Env> {
    let _ = (from, state, ctx);
    succeed((data, Vec::new()))
  }
}

/// What [`Statem::init`] returns: initial state, data and actions.
pub type StatemInit<M> = (<M as Statem>::State, <M as Statem>::Data, Vec<Action<M>>);

/// What [`Statem::on_enter`] returns: the data and actions.
pub type Entered<M> = (<M as Statem>::Data, Vec<Action<M>>);

/// The outcome of [`Statem::handle_event`].
#[non_exhaustive]
pub enum Transition<M: Statem> {
  /// Move to a state (a state change when it differs from the current one).
  Next(M::State, M::Data, Vec<Action<M>>),
  /// Stay in the current state.
  Keep(M::Data, Vec<Action<M>>),
  /// Stop the process with `reason`; pending postponed calls see the exit as
  /// [`CallError::Down`].
  Stop(ExitReason),
}

/// A transition action, run in order after the callback that returned it.
#[non_exhaustive]
pub enum Action<M: Statem> {
  /// Re-deliver the current event after the next state change.
  Postpone,
  /// Deliver [`StatemEvent::StateTimeout`] after the duration unless the state changes first.
  StateTimeout(Duration, M::Event),
  /// Cancel the pending state timeout.
  CancelStateTimeout,
  /// Deliver [`StatemEvent::EventTimeout`] after the duration unless any event arrives first.
  EventTimeout(Duration, M::Event),
  /// Deliver [`StatemEvent::GenericTimeout`] after the duration; replaces a timer of that name.
  GenericTimeout(&'static str, Duration, M::Event),
  /// Cancel the named generic timeout.
  CancelGenericTimeout(&'static str),
  /// Answer a call.
  Reply(ReplyTo<M::Reply>, M::Reply),
  /// Handle this event (as [`StatemEvent::Internal`]) before any other.
  NextEvent(M::Event),
}

/// An event delivered to [`Statem::handle_event`], tagged with how it arrived.
#[non_exhaustive]
pub enum StatemEvent<M: Statem> {
  /// [`StatemHandle::call`]: answer with [`Action::Reply`].
  Call(M::Event, ReplyTo<M::Reply>),
  /// [`StatemHandle::cast`] / `Addr::cast`.
  Cast(M::Event),
  /// [`StatemHandle::send`] / `Addr::send(StatemMsg::info(..))`.
  Info(M::Event),
  /// An [`Action::StateTimeout`] fired.
  StateTimeout(M::Event),
  /// An [`Action::EventTimeout`] fired.
  EventTimeout(M::Event),
  /// The named [`Action::GenericTimeout`] fired.
  GenericTimeout(&'static str, M::Event),
  /// Inserted with [`Action::NextEvent`].
  Internal(M::Event),
}

impl<M: Statem> StatemEvent<M> {
  /// The event payload, whatever its origin.
  pub fn content(&self) -> &M::Event {
    match self {
      StatemEvent::Call(e, _)
      | StatemEvent::Cast(e)
      | StatemEvent::Info(e)
      | StatemEvent::StateTimeout(e)
      | StatemEvent::EventTimeout(e)
      | StatemEvent::GenericTimeout(_, e)
      | StatemEvent::Internal(e) => e,
    }
  }

  /// The caller to answer, for a [`StatemEvent::Call`].
  pub fn reply_to(&self) -> Option<&ReplyTo<M::Reply>> {
    match self {
      StatemEvent::Call(_, to) => Some(to),
      _ => None,
    }
  }
}

/// The waiting caller of a [`StatemHandle::call`]. Cloneable, so a handler can reply from
/// a later event (for example after postponing the call).
pub struct ReplyTo<A> {
  reply: Deferred<A, CallError>,
}

impl<A> Clone for ReplyTo<A> {
  fn clone(&self) -> Self {
    Self {
      reply: self.reply.clone(),
    }
  }
}

impl<A> Debug for ReplyTo<A> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ReplyTo").finish_non_exhaustive()
  }
}

impl<A: Clone + Send + 'static> ReplyTo<A> {
  /// Answer the caller; `false` if it was already answered. A caller that timed out ignores
  /// the reply.
  pub fn reply(&self, value: A) -> bool {
    self.reply.try_succeed(value)
  }
}

/// The `Info` message type of a [`StatemProcess`]. Build one with [`StatemMsg::info`]; calls
/// and timer messages are internal.
pub struct StatemMsg<M: Statem>(Msg<M>);

enum Msg<M: Statem> {
  Info(M::Event),
  Call(M::Event, ReplyTo<M::Reply>),
  Timeout(Slot, u64, M::Event),
}

impl<M: Statem> StatemMsg<M> {
  /// A message delivered as [`StatemEvent::Info`].
  pub fn info(event: M::Event) -> Self {
    Self(Msg::Info(event))
  }
}

/// Synchronous queries a [`StatemProcess`] answers itself (not machine events).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatemQuery {
  /// Read the current state.
  State,
}

/// Which timer a timeout message belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
  State,
  Event,
  Generic(&'static str),
}

/// A pending timer and its generation id (a message with another id is stale).
struct Armed {
  id: u64,
  timer: TimerRef,
}

/// The process hosting a [`Statem`] `M`. Start it with [`StatemProcess::start`] /
/// [`StatemProcess::start_linked`], or supervise it with
/// `ChildSpec::worker::<StatemProcess<M>, _>(id, move || (args.clone(), env.clone()))`
/// (`Args` is `M::Args`, `Env` is `M::Env`); a restart runs [`Statem::init`] again.
pub struct StatemProcess<M: Statem> {
  state: M::State,
  /// `None` only after [`Transition::Stop`] consumed it.
  data: Option<M::Data>,
  /// Events to handle before the next mailbox message (inserted, then re-delivered).
  queue: VecDeque<StatemEvent<M>>,
  postponed: VecDeque<StatemEvent<M>>,
  state_timer: Option<Armed>,
  event_timer: Option<Armed>,
  generic: HashMap<&'static str, Armed>,
  timer_seq: u64,
}

type Handled<M> = Effect<Next<StatemProcess<M>>, <M as Statem>::Error, <M as Statem>::Env>;
type EnterOutcome<M> =
  Effect<(StatemProcess<M>, Result<(), ExitReason>), <M as Statem>::Error, <M as Statem>::Env>;

/// The exit reason for an action that has no meaning where it was returned.
fn bad_action(what: &str) -> ExitReason {
  ExitReason::Error(format!("statem: {what}"))
}

/// Crash `init` (which cannot stop with a reason): the process exits with
/// `ExitReason::Error("panic: ..")`.
fn crash<A, E, R>(reason: ExitReason) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  suspend(move || -> Effect<A, E, R> { panic!("{reason:?}") })
}

impl<M: Statem> StatemProcess<M> {
  /// Spawn an unlinked machine on `node`.
  pub fn start(node: &ProcessNode, args: M::Args, env: M::Env) -> StatemHandle<M> {
    StatemHandle::from_addr(node, node.spawn::<Self>(args, env))
  }

  /// Spawn a machine linked to the calling process.
  pub fn start_linked(ctx: &ProcessCtx, args: M::Args, env: M::Env) -> StatemHandle<M> {
    StatemHandle::from_addr(ctx.node(), ctx.spawn_link::<Self>(args, env))
  }

  fn new(state: M::State, data: M::Data) -> Self {
    Self {
      state,
      data: Some(data),
      queue: VecDeque::new(),
      postponed: VecDeque::new(),
      state_timer: None,
      event_timer: None,
      generic: HashMap::new(),
      timer_seq: 0,
    }
  }

  fn arm(&mut self, slot: Slot, after: Duration, event: M::Event, ctx: &ProcessCtx) -> Armed {
    self.timer_seq += 1;
    let id = self.timer_seq;
    let timer = ctx.send_after::<Self>(StatemMsg(Msg::Timeout(slot, id, event)), after);
    Armed { id, timer }
  }

  fn disarm(armed: Option<Armed>, ctx: &ProcessCtx) {
    if let Some(armed) = armed {
      ctx.cancel_timer(armed.timer);
    }
  }

  /// Take timer `id` off its slot; `false` for a stale message (its timer was replaced or
  /// cancelled after the message was queued).
  fn claim(&mut self, slot: Slot, id: u64) -> bool {
    let armed = match slot {
      Slot::State => &mut self.state_timer,
      Slot::Event => &mut self.event_timer,
      Slot::Generic(name) => {
        let live = self.generic.get(name).is_some_and(|a| a.id == id);
        if live {
          self.generic.remove(name);
        }
        return live;
      }
    };
    let live = armed.as_ref().is_some_and(|a| a.id == id);
    if live {
      *armed = None;
    }
    live
  }

  /// Run `actions` in order. `event` is the event being handled (`None` in `init` and state
  /// enter); inserted events are collected into `inserted`.
  fn run_actions(
    &mut self,
    actions: Vec<Action<M>>,
    event: &mut Option<StatemEvent<M>>,
    inserted: &mut Vec<M::Event>,
    ctx: &ProcessCtx,
  ) -> Result<(), ExitReason> {
    for action in actions {
      match action {
        Action::Postpone => match event.take() {
          Some(e) => self.postponed.push_back(e),
          None => return Err(bad_action("Postpone with no event to postpone")),
        },
        Action::StateTimeout(after, msg) => {
          let armed = self.arm(Slot::State, after, msg, ctx);
          Self::disarm(self.state_timer.replace(armed), ctx);
        }
        Action::CancelStateTimeout => Self::disarm(self.state_timer.take(), ctx),
        Action::EventTimeout(after, msg) => {
          let armed = self.arm(Slot::Event, after, msg, ctx);
          Self::disarm(self.event_timer.replace(armed), ctx);
        }
        Action::GenericTimeout(name, after, msg) => {
          let armed = self.arm(Slot::Generic(name), after, msg, ctx);
          Self::disarm(self.generic.insert(name, armed), ctx);
        }
        Action::CancelGenericTimeout(name) => Self::disarm(self.generic.remove(name), ctx),
        Action::Reply(to, value) => {
          to.reply(value);
        }
        Action::NextEvent(e) => inserted.push(e),
      }
    }
    Ok(())
  }

  /// Actions from `init` or a state enter: no event to postpone, no events to insert.
  fn run_setup(
    &mut self,
    actions: Vec<Action<M>>,
    origin: &str,
    ctx: &ProcessCtx,
  ) -> Result<(), ExitReason> {
    let mut inserted = Vec::new();
    self.run_actions(actions, &mut None, &mut inserted, ctx)?;
    if inserted.is_empty() {
      Ok(())
    } else {
      Err(bad_action(&format!("NextEvent in {origin}")))
    }
  }

  /// Queue `inserted` events, then `retried` postponed ones, ahead of the rest.
  fn requeue(&mut self, inserted: Vec<M::Event>, retried: VecDeque<StatemEvent<M>>) {
    let rest = std::mem::take(&mut self.queue);
    self.queue = inserted
      .into_iter()
      .map(StatemEvent::Internal)
      .chain(retried)
      .chain(rest)
      .collect();
  }

  fn enter(mut self, from: Option<M::State>, ctx: ProcessCtx) -> EnterOutcome<M> {
    let Some(data) = self.data.take() else {
      return succeed((self, Err(bad_action("data missing"))));
    };
    let entered = M::on_enter(from.as_ref(), &self.state, data, ctx.clone());
    entered.map(move |(data, actions)| {
      self.data = Some(data);
      let outcome = self.run_setup(actions, "a state enter callback", &ctx);
      (self, outcome)
    })
  }

  fn deliver(mut self, event: StatemEvent<M>, ctx: ProcessCtx) -> Handled<M> {
    self.queue.push_back(event);
    self.drive(ctx)
  }

  /// Handle queued events until none is left.
  fn drive(mut self, ctx: ProcessCtx) -> Handled<M> {
    let Some(event) = self.queue.pop_front() else {
      return succeed(Next::Continue(self));
    };
    // Any event cancels the event timeout (one that fired was already taken off its slot).
    Self::disarm(self.event_timer.take(), &ctx);
    let Some(data) = self.data.take() else {
      return succeed(Next::Stop(bad_action("data missing"), self));
    };
    let handled = M::handle_event(&event, &self.state, data, ctx.clone());
    handled.flat_map(move |transition| self.apply(event, transition, ctx))
  }

  fn apply(
    mut self,
    event: StatemEvent<M>,
    transition: Transition<M>,
    ctx: ProcessCtx,
  ) -> Handled<M> {
    let (next_state, data, actions) = match transition {
      Transition::Next(state, data, actions) => (Some(state), data, actions),
      Transition::Keep(data, actions) => (None, data, actions),
      Transition::Stop(reason) => return succeed(Next::Stop(reason, self)),
    };
    self.data = Some(data);
    let changed = next_state.as_ref().is_some_and(|s| *s != self.state);
    // Cancel before the actions run, so a `StateTimeout` returned with the change arms the
    // new state's timeout.
    if changed {
      Self::disarm(self.state_timer.take(), &ctx);
    }
    let mut event = Some(event);
    let mut inserted = Vec::new();
    if let Err(reason) = self.run_actions(actions, &mut event, &mut inserted, &ctx) {
      return succeed(Next::Stop(reason, self));
    }
    let from = next_state.map(|state| std::mem::replace(&mut self.state, state));
    let Some(from) = from.filter(|_| changed) else {
      self.requeue(inserted, VecDeque::new());
      return self.drive(ctx);
    };
    let retried = std::mem::take(&mut self.postponed);
    self.requeue(inserted, retried);
    self
      .enter(Some(from), ctx.clone())
      .flat_map(move |(this, entered)| match entered {
        Ok(()) => this.drive(ctx),
        Err(reason) => succeed(Next::Stop(reason, this)),
      })
  }
}

impl<M: Statem> Process for StatemProcess<M> {
  type Args = M::Args;
  type Env = M::Env;
  type Call = StatemQuery;
  type Reply = M::State;
  type Cast = M::Event;
  type Info = StatemMsg<M>;
  type Error = M::Error;

  fn init(args: M::Args, ctx: ProcessCtx) -> Effect<Self, M::Error, M::Env> {
    M::init(args, ctx.clone()).flat_map(move |(state, data, actions)| {
      let mut this = Self::new(state, data);
      if let Err(reason) = this.run_setup(actions, "init", &ctx) {
        return crash(reason);
      }
      this
        .enter(None, ctx)
        .flat_map(|(this, entered)| match entered {
          Ok(()) => succeed(this),
          Err(reason) => crash(reason),
        })
    })
  }

  fn handle_call(
    self,
    msg: StatemQuery,
    _ctx: ProcessCtx,
  ) -> Effect<(M::State, Next<Self>), M::Error, M::Env> {
    match msg {
      StatemQuery::State => {
        let state = self.state.clone();
        succeed((state, Next::Continue(self)))
      }
    }
  }

  fn handle_cast(self, event: M::Event, ctx: ProcessCtx) -> Handled<M> {
    self.deliver(StatemEvent::Cast(event), ctx)
  }

  fn handle_info(mut self, msg: Info<StatemMsg<M>>, ctx: ProcessCtx) -> Handled<M> {
    let Info::User(StatemMsg(msg)) = msg else {
      return succeed(Next::Continue(self));
    };
    let event = match msg {
      Msg::Info(e) => StatemEvent::Info(e),
      Msg::Call(e, to) => StatemEvent::Call(e, to),
      Msg::Timeout(slot, id, e) => {
        if !self.claim(slot, id) {
          return succeed(Next::Continue(self));
        }
        match slot {
          Slot::State => StatemEvent::StateTimeout(e),
          Slot::Event => StatemEvent::EventTimeout(e),
          Slot::Generic(name) => StatemEvent::GenericTimeout(name, e),
        }
      }
    };
    self.deliver(event, ctx)
  }
}

/// Typed handle to a running [`StatemProcess`].
pub struct StatemHandle<M: Statem> {
  addr: Addr<StatemProcess<M>>,
  node: ProcessNode,
}

impl<M: Statem> Clone for StatemHandle<M> {
  fn clone(&self) -> Self {
    Self {
      addr: self.addr.clone(),
      node: self.node.clone(),
    }
  }
}

impl<M: Statem> StatemHandle<M> {
  /// Wrap the address of a machine running on `node` (e.g. from a supervisor start closure).
  pub fn from_addr(node: &ProcessNode, addr: Addr<StatemProcess<M>>) -> Self {
    Self {
      addr,
      node: node.clone(),
    }
  }

  /// The process pid.
  pub fn pid(&self) -> &Pid {
    self.addr.pid()
  }

  /// The raw typed address.
  pub fn addr(&self) -> &Addr<StatemProcess<M>> {
    &self.addr
  }

  /// Current state (answered by the process, not a machine event).
  pub fn state<Rn: Send + Sync + 'static>(
    &self,
    timeout: Duration,
  ) -> Effect<M::State, CallError, Rn> {
    self.addr.call(StatemQuery::State, timeout)
  }

  /// Deliver `event` as [`StatemEvent::Call`] and wait for the [`Action::Reply`]. The reply may
  /// come from a later event (a postponed call); the machine exiting first fails the call with
  /// [`CallError::Down`], the deadline with [`CallError::Timeout`] (at-most-once).
  pub fn call<Rn: Send + Sync + 'static>(
    &self,
    event: M::Event,
    timeout: Duration,
  ) -> Effect<M::Reply, CallError, Rn> {
    let addr = self.addr.clone();
    let node = self.node.clone();
    suspend(move || {
      let pid = addr.pid().clone();
      let reply: Deferred<M::Reply, CallError> = Deferred::new();
      let down = reply.clone();
      let monitor = node.monitor_with_sink(
        &pid,
        Box::new(move |_m, _p, reason: ExitReason| {
          down.try_fail_cause(Cause::Fail(CallError::Down(reason)));
        }),
      );
      let to = ReplyTo {
        reply: reply.clone(),
      };
      if !addr.send_now(StatemMsg(Msg::Call(event, to))) {
        node.demonitor(&pid, monitor);
        return fail(if addr.is_alive() {
          CallError::MailboxFull
        } else {
          CallError::MailboxClosed
        });
      }
      reply
        .wait()
        .timeout_fail(timeout, || CallError::Timeout)
        .ensuring(sync(move || node.demonitor(&pid, monitor)))
    })
  }

  /// Deliver `event` as [`StatemEvent::Cast`].
  pub fn cast<Rn: Send + Sync + 'static>(&self, event: M::Event) -> Effect<(), SendError, Rn> {
    self.addr.cast(event)
  }

  /// Deliver `event` as [`StatemEvent::Info`].
  pub fn send<Rn: Send + Sync + 'static>(&self, event: M::Event) -> Effect<(), SendError, Rn> {
    self.addr.send(StatemMsg::info(event))
  }
}
