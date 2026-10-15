//! [`FsmProcess`] — a `gen_statem`-style process behaviour hosting a [`StateMachine`].
//!
//! Events arrive as mailbox messages and are applied through the machine's
//! [`TransitionTable`](crate::TransitionTable); an optional [`Interpreter`] supplies
//! [`Effect`]-valued transition actions that run inside the process (with the process
//! environment `R`) before the new state is committed. The behaviour implements
//! [`id_effect_process::Process`], so it is spawned, linked, monitored and supervised like
//! any other process.
//!
//! | Message | Effect |
//! |---------|--------|
//! | [`FsmHandle::state`] (call) | Reply with the current state |
//! | [`FsmHandle::fire`] (call) | Apply the event; reply with the new state or the [`FsmError`] |
//! | [`FsmHandle::cast`] / `Addr::send` | Apply the event; unhandled events follow [`Unhandled`] |
//!
//! A failing transition action crashes the process with [`ExitReason::Error`] (the state is not
//! advanced), so a supervisor can restart it from its spec — "let it crash" for state machines.
//!
//! `id_effect_process` runs on the 0.7 `next` kernel; the transition actions are still v1
//! effects over `&mut R` (until `m-fsm`). The process therefore owns `R` and runs an action on
//! the executor's blocking pool (`on_blocking_pool`), handing the machine and `R` back when it
//! is done.

use std::any::Any;
use std::fmt::Debug;
use std::hash::Hash;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::time::Duration;

use id_effect::next::{Effect as NextEffect, HNil, die, fail, succeed, sync};
use id_effect::{Effect, box_future, run_blocking};
use id_effect_process::{
  Addr, CallError, ExitReason, Info, Mailbox, Next, Pid, Process, ProcessCtx, ProcessNode,
  SendError,
};

use crate::definition::Fsm;
use crate::error::FsmError;
use crate::interpreter::Interpreter;
use crate::machine::StateMachine;

/// What a running [`FsmProcess`] does with a cast/sent event that has no transition from the
/// current state. Calls always reply with [`FsmReply::Rejected`] and keep running.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Unhandled {
  /// Drop the event and stay in the current state.
  #[default]
  Ignore,
  /// Stop the process with [`ExitReason::Error`] describing the rejected event.
  Stop,
}

/// Start arguments for an [`FsmProcess`]: machine, actions, and unhandled-event policy.
///
/// `Clone`, so it can back a re-runnable supervisor `ChildSpec` factory.
pub struct FsmSpec<S, E, Err, R>
where
  S: Copy + Eq + Hash + 'static,
  E: Copy + Eq + Hash + 'static,
  Err: 'static,
  R: 'static,
{
  machine: StateMachine<S, E>,
  interpreter: Interpreter<S, E, (), Err, R>,
  unhandled: Unhandled,
}

impl<S, E, Err, R> Clone for FsmSpec<S, E, Err, R>
where
  S: Copy + Eq + Hash + 'static,
  E: Copy + Eq + Hash + 'static,
  Err: 'static,
  R: 'static,
{
  fn clone(&self) -> Self {
    Self {
      machine: self.machine.clone(),
      interpreter: self.interpreter.clone(),
      unhandled: self.unhandled,
    }
  }
}

impl<S, E, Err, R> FsmSpec<S, E, Err, R>
where
  S: Copy + Eq + Hash + Debug + 'static,
  E: Copy + Eq + Hash + Debug + 'static,
  Err: 'static,
  R: 'static,
{
  /// Spec for `machine` with no transition actions and [`Unhandled::Ignore`].
  pub fn new(machine: StateMachine<S, E>) -> Self {
    Self {
      machine,
      interpreter: Interpreter::new(),
      unhandled: Unhandled::Ignore,
    }
  }

  /// Spec for a fresh [`Fsm::state_machine`].
  pub fn from_fsm() -> Self
  where
    S: Fsm<Event = E>,
  {
    Self::new(S::state_machine())
  }

  /// Run `interpreter`'s actions on matching transitions.
  pub fn with_interpreter(mut self, interpreter: Interpreter<S, E, (), Err, R>) -> Self {
    self.interpreter = interpreter;
    self
  }

  /// Set the policy for cast/sent events without a transition.
  pub fn on_unhandled(mut self, unhandled: Unhandled) -> Self {
    self.unhandled = unhandled;
    self
  }
}

/// Synchronous requests understood by an [`FsmProcess`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsmCall<E> {
  /// Read the current state.
  State,
  /// Apply an event.
  Fire(E),
}

/// Replies from an [`FsmProcess`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsmReply<S, E> {
  /// Current state (reply to [`FsmCall::State`]).
  State(S),
  /// The event was applied; this is the new state.
  Transitioned(S),
  /// No transition exists for the event from the current state; nothing changed.
  Rejected(FsmError<S, E>),
}

/// The `gen_statem`-style behaviour (see the module docs). Start it with
/// [`FsmProcess::start`] / [`FsmProcess::start_linked`], or supervise it with
/// `ChildSpec::worker::<FsmProcess<S, E, Err, R>, _>(id, || ((spec.clone(), env), HNil))`.
pub struct FsmProcess<S, E, Err, R>
where
  S: Copy + Eq + Hash + 'static,
  E: Copy + Eq + Hash + 'static,
  Err: 'static,
  R: 'static,
{
  spec: FsmSpec<S, E, Err, R>,
  /// The actions' v1 environment, owned by the process.
  env: R,
}

type Fired<S, E, Err, R> =
  NextEffect<(Result<S, FsmError<S, E>>, FsmProcess<S, E, Err, R>), Err, HNil>;

/// Runs `job` on the executor's blocking pool and resumes with its result (a panic in `job` is
/// re-raised in the fiber, so it becomes a defect). The adapter that lets a `next` process run a
/// v1 transition action, which needs a thread it may block.
fn on_blocking_pool<T, Err>(
  job: impl FnOnce() -> Result<T, Err> + Send + 'static,
) -> NextEffect<T, Err, HNil>
where
  T: Send + 'static,
  Err: Send + 'static,
{
  type Outcome<T, Err> = Result<Result<T, Err>, Box<dyn Any + Send>>;
  let slot: Mailbox<Outcome<T, Err>> = Mailbox::new();
  let tx = slot.clone();
  NextEffect::<(), Err, HNil>::from_ctx(move |ctx| {
    ctx.fiber().executor().spawn_blocking(Box::new(move || {
      tx.offer_now(catch_unwind(AssertUnwindSafe(job)));
    }));
    async { Ok(()) }
  })
  .then(
    slot
      .take()
      .catch(|_| die("fsm: blocking action dropped its result")),
  )
  .flat_map(|outcome| match outcome {
    Ok(Ok(t)) => succeed(t),
    Ok(Err(e)) => fail(e),
    Err(payload) => sync(move || resume_unwind(payload)),
  })
}

impl<S, E, Err, R> FsmProcess<S, E, Err, R>
where
  S: Copy + Eq + Hash + Debug + Send + Sync + 'static,
  E: Copy + Eq + Hash + Debug + Send + Sync + 'static,
  Err: Debug + Clone + Send + Sync + 'static,
  R: Send + 'static,
{
  /// Spawn an unlinked state-machine process on `node`.
  pub fn start(node: &ProcessNode, spec: FsmSpec<S, E, Err, R>, env: R) -> FsmHandle<S, E, Err, R> {
    FsmHandle::from_addr(node.spawn::<Self>((spec, env), HNil))
  }

  /// Spawn a state-machine process linked to the calling process.
  pub fn start_linked(
    ctx: &ProcessCtx,
    spec: FsmSpec<S, E, Err, R>,
    env: R,
  ) -> FsmHandle<S, E, Err, R> {
    FsmHandle::from_addr(ctx.spawn_link::<Self>((spec, env), HNil))
  }

  /// Apply `event`: reject without side effects when there is no edge, otherwise run the
  /// registered action (if any) and commit the new state.
  fn fire(mut self, event: E) -> Fired<S, E, Err, R> {
    let from = self.spec.machine.state();
    let Some(to) = self.spec.machine.table().next(from, event) else {
      let rejected = FsmError::NoTransition { state: from, event };
      return succeed((Err(rejected), self));
    };
    if !self.spec.interpreter.has_action(from, event) {
      self.spec.machine.set_state(to);
      return succeed((Ok(to), self));
    }
    // The v1 action is built and run on the blocking thread (v1 effects are not `Send`).
    on_blocking_pool(move || {
      let FsmProcess { mut spec, env } = self;
      let env = match spec.interpreter.action(from, event) {
        // The action borrows `R` mutably; the wrapper owns it and hands it back on success
        // (on failure the process crashes, so `R` goes with it).
        Some(action) => run_blocking(
          Effect::new_async(move |_: &mut ()| {
            box_future(async move {
              let mut env = env;
              action.run(&mut env).await?;
              Ok(env)
            })
          }),
          (),
        )?,
        None => env,
      };
      spec.machine.set_state(to);
      Ok((Ok(to), FsmProcess { spec, env }))
    })
  }

  fn fire_async(self, event: E) -> NextEffect<Next<Self>, Err, HNil> {
    self.fire(event).map(|(outcome, this)| match outcome {
      Err(rejected) if this.spec.unhandled == Unhandled::Stop => {
        Next::Stop(ExitReason::error(rejected), this)
      }
      _ => Next::Continue(this),
    })
  }
}

impl<S, E, Err, R> Process for FsmProcess<S, E, Err, R>
where
  S: Copy + Eq + Hash + Debug + Send + Sync + 'static,
  E: Copy + Eq + Hash + Debug + Send + Sync + 'static,
  Err: Debug + Clone + Send + Sync + 'static,
  R: Send + 'static,
{
  type Args = (FsmSpec<S, E, Err, R>, R);
  type Env = HNil;
  type Call = FsmCall<E>;
  type Reply = FsmReply<S, E>;
  type Cast = E;
  type Info = E;
  type Error = Err;

  fn init(
    (spec, env): (FsmSpec<S, E, Err, R>, R),
    _ctx: ProcessCtx,
  ) -> NextEffect<Self, Err, HNil> {
    succeed(FsmProcess { spec, env })
  }

  fn handle_call(
    self,
    msg: FsmCall<E>,
    _ctx: ProcessCtx,
  ) -> NextEffect<(FsmReply<S, E>, Next<Self>), Err, HNil> {
    match msg {
      FsmCall::State => {
        let state = self.spec.machine.state();
        succeed((FsmReply::State(state), Next::Continue(self)))
      }
      FsmCall::Fire(event) => self.fire(event).map(|(outcome, this)| {
        let reply = match outcome {
          Ok(to) => FsmReply::Transitioned(to),
          Err(rejected) => FsmReply::Rejected(rejected),
        };
        (reply, Next::Continue(this))
      }),
    }
  }

  fn handle_cast(self, event: E, _ctx: ProcessCtx) -> NextEffect<Next<Self>, Err, HNil> {
    self.fire_async(event)
  }

  fn handle_info(self, msg: Info<E>, _ctx: ProcessCtx) -> NextEffect<Next<Self>, Err, HNil> {
    match msg {
      Info::User(event) => self.fire_async(event),
      _ => succeed(Next::Continue(self)),
    }
  }
}

/// Typed handle to a running [`FsmProcess`].
pub struct FsmHandle<S, E, Err, R>
where
  S: Copy + Eq + Hash + Debug + Send + Sync + 'static,
  E: Copy + Eq + Hash + Debug + Send + Sync + 'static,
  Err: Debug + Clone + Send + Sync + 'static,
  R: Send + 'static,
{
  addr: Addr<FsmProcess<S, E, Err, R>>,
}

impl<S, E, Err, R> Clone for FsmHandle<S, E, Err, R>
where
  S: Copy + Eq + Hash + Debug + Send + Sync + 'static,
  E: Copy + Eq + Hash + Debug + Send + Sync + 'static,
  Err: Debug + Clone + Send + Sync + 'static,
  R: Send + 'static,
{
  fn clone(&self) -> Self {
    Self {
      addr: self.addr.clone(),
    }
  }
}

impl<S, E, Err, R> FsmHandle<S, E, Err, R>
where
  S: Copy + Eq + Hash + Debug + Send + Sync + 'static,
  E: Copy + Eq + Hash + Debug + Send + Sync + 'static,
  Err: Debug + Clone + Send + Sync + 'static,
  R: Send + 'static,
{
  /// Wrap a typed address (e.g. one obtained from a supervisor start closure or the registry).
  pub fn from_addr(addr: Addr<FsmProcess<S, E, Err, R>>) -> Self {
    Self { addr }
  }

  /// The process pid.
  pub fn pid(&self) -> &Pid {
    self.addr.pid()
  }

  /// The raw typed address.
  pub fn addr(&self) -> &Addr<FsmProcess<S, E, Err, R>> {
    &self.addr
  }

  /// Current state.
  pub fn state<Rn: Send + Sync + 'static>(
    &self,
    timeout: Duration,
  ) -> NextEffect<S, CallError, Rn> {
    self
      .addr
      .call(FsmCall::State, timeout)
      .map(|reply| match reply {
        FsmReply::State(s) | FsmReply::Transitioned(s) => s,
        FsmReply::Rejected(FsmError::NoTransition { state, .. }) => state,
      })
  }

  /// Apply `event` and wait for the outcome: the new state, or the rejection when no
  /// transition exists. A failing transition action surfaces as [`CallError::Down`].
  pub fn fire<Rn: Send + Sync + 'static>(
    &self,
    event: E,
    timeout: Duration,
  ) -> NextEffect<Result<S, FsmError<S, E>>, CallError, Rn> {
    self
      .addr
      .call(FsmCall::Fire(event), timeout)
      .map(|reply| match reply {
        FsmReply::Transitioned(s) | FsmReply::State(s) => Ok(s),
        FsmReply::Rejected(rejected) => Err(rejected),
      })
  }

  /// Fire-and-forget `event` (applied in mailbox order).
  pub fn cast<Rn: Send + Sync + 'static>(&self, event: E) -> NextEffect<(), SendError, Rn> {
    self.addr.cast(event)
  }
}
